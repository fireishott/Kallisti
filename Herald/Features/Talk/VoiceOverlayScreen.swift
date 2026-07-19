import SwiftUI
import UIKit

/// Full-screen voice overlay, inspired by ChatGPT's voice mode.
/// Auto-starts a voice session on appear and tears it down on dismiss.
struct VoiceOverlayScreen: View {
    @Environment(TalkStore.self) private var talkStore
    @Environment(TabRouter.self) private var router

    @State private var showLiveCameraOverlay = false

    var body: some View {
        ZStack {
            Design.Colors.background
                .ignoresSafeArea()

            VStack(spacing: 0) {
                // Top bar
                HStack(alignment: .firstTextBaseline, spacing: Design.Spacing.xs) {
                    Text("HERMES")
                        .font(Design.Typography.headline)
                        .tracking(1.0)
                        .foregroundStyle(Design.Colors.foreground)
                    Text("· voice")
                        .font(Design.Typography.headline)
                        .foregroundStyle(Design.Colors.secondaryForeground)
                    Spacer()
                }
                .padding(.horizontal, Design.Spacing.lg)
                .padding(.top, Design.Spacing.md)

                Spacer()

                // Transcript area
                transcriptSection

                Spacer()

                // Voice orb
                VoiceOrb(voiceState: talkStore.voiceState, connectionState: talkStore.connectionState)
                    .onTapGesture {
                        if talkStore.voiceState == .speaking {
                            talkStore.interruptAssistant()
                        }
                    }
                    .padding(.bottom, Design.Spacing.sm)

                // Status label — always visible, adapts to state
                orbStatusLabel
                    .padding(.horizontal, Design.Spacing.xl)
                    .animation(Design.Motion.quickResponse, value: talkStore.connectionState)
                    .animation(Design.Motion.quickResponse, value: talkStore.voiceState)

                Spacer()

                // Bottom controls
                controlBar
                    .padding(.bottom, Design.Spacing.xxl)
            }
        }
        .task {
            // Skip the readiness check — go straight to session create.
            // If the host is offline or unconfigured, session create fails
            // with a clear error. This saves 2-4s of startup latency
            // (the prewarm RPC rebuilds voice context from disk + subprocess).
            await talkStore.startSessionDirectly()
        }
        .onDisappear {
            // Always clean up the voice session when the overlay disappears.
            // Use a short delay to avoid killing the session when the camera
            // fullScreenCover appears (which triggers onDisappear transiently).
            if talkStore.isSessionActive {
                Task {
                    try? await Task.sleep(for: .milliseconds(500))
                    // Re-check — if the overlay was re-presented (camera dismiss),
                    // the session is still wanted. Only end if truly gone.
                    if !showLiveCameraOverlay {
                        await talkStore.endSession()
                    }
                }
            }
        }
        .statusBarHidden(true)
        .fullScreenCover(isPresented: $showLiveCameraOverlay) {
            LiveCameraOverlay(
                onFrameCaptured: { frameData, _ in
                    // Send frames silently — model responds when user speaks
                    talkStore.sendImage(frameData, triggerResponse: false)
                },
                onDismiss: {
                    showLiveCameraOverlay = false
                }
            )
        }
    }

    // MARK: - Transcript

    private var transcriptSection: some View {
        ScrollView {
            LazyVStack(spacing: Design.Spacing.sm) {
                ForEach(talkStore.transcriptItems) { item in
                    transcriptBubble(item)
                }
            }
            .padding(.horizontal, Design.Spacing.lg)
        }
        .scrollDismissesKeyboard(.never)
        .defaultScrollAnchor(.bottom)
        .frame(maxHeight: 320)
    }

    @ViewBuilder
    private func transcriptBubble(_ item: TranscriptItem) -> some View {
        switch item.speaker {
        case .user:
            HStack {
                Spacer()
                if let imageData = item.imageData, let uiImage = UIImage(data: imageData) {
                    Image(uiImage: uiImage)
                        .resizable()
                        .aspectRatio(contentMode: .fill)
                        .frame(width: 80, height: 80)
                        .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.md))
                } else if !item.text.isEmpty {
                    Text("\u{201C}\(item.text)\u{201D}")
                        .font(Design.Typography.editorialItalicSmall)
                        .foregroundStyle(Design.Colors.foreground)
                        .multilineTextAlignment(.trailing)
                        .opacity(item.isPartial ? 0.55 : 1)
                }
            }
        case .herald:
            HStack {
                Text(item.text)
                    .font(Design.Typography.body)
                    .foregroundStyle(Design.Colors.foreground)
                    .opacity(item.isPartial ? 0.6 : 1)
                Spacer()
            }
        case .system:
            Text(item.text)
                .brandEyebrow()
                .frame(maxWidth: .infinity)
        }
    }

    // MARK: - Orb Status

    @ViewBuilder
    private var orbStatusLabel: some View {
        switch (talkStore.connectionState, talkStore.voiceState) {
        case (.failed, _), (.blocked, _):
            VStack(spacing: Design.Spacing.sm) {
                Text(talkStore.blockedReason ?? "Unable to connect")
                    .font(Design.Typography.callout)
                    .foregroundStyle(Design.Colors.secondaryForeground)
                    .multilineTextAlignment(.center)

                // Show "Open Settings" for permission-related blocks
                if let reason = talkStore.blockedReason,
                   reason.localizedCaseInsensitiveContains("microphone") || reason.localizedCaseInsensitiveContains("permission") {
                    Button {
                        if let url = URL(string: UIApplication.openSettingsURLString) {
                            UIApplication.shared.open(url)
                        }
                    } label: {
                        Text("Open Settings")
                            .brandEyebrow(Design.Colors.foreground)
                            .padding(.horizontal, Design.Spacing.lg)
                            .padding(.vertical, Design.Spacing.xs)
                            .background(Design.Colors.surface)
                            .overlay(
                                Capsule().stroke(Design.Colors.border, lineWidth: 1)
                            )
                            .clipShape(Capsule())
                    }
                }
            }

        case (.checking, _), (.idle, _), (.connecting, _), (.ready, _):
            HStack(spacing: Design.Spacing.xs) {
                ProgressView()
                    .controlSize(.small)
                    .tint(Design.Colors.secondaryForeground)
                Text("Connecting\u{2026}")
                    .brandEyebrow()
            }

        case (.connected, .listening):
            orbStatus(dot: Design.Colors.success, label: "Herald · Listening")

        case (.connected, .thinking):
            if let status = talkStore.statusMessage, !status.isEmpty {
                orbStatus(dot: Design.Brand.primary, label: status)
            } else {
                orbStatus(dot: Design.Brand.primary, label: "Herald · Thinking")
            }

        case (.connected, .speaking):
            orbStatus(dot: Design.Brand.accent, label: "Herald · Speaking")

        case (_, .disconnected):
            orbStatus(dot: Design.Colors.warning, label: "Disconnected")

        default:
            EmptyView()
        }
    }

    /// `● hermes · speaking` status strip — the signature brand metadata line.
    private func orbStatus(dot: Color, label: String) -> some View {
        HStack(spacing: Design.Spacing.xs) {
            Circle()
                .fill(dot)
                .frame(width: 6, height: 6)
            Text(label)
                .brandEyebrow(Design.Colors.foreground)
        }
    }

    // MARK: - Controls

    private var controlBar: some View {
        HStack(spacing: Design.Spacing.xl) {
            if talkStore.isSessionActive {
                // Live camera button
                voiceControlButton(
                    systemImage: "video.fill",
                    accessibilityLabel: "Open live camera"
                ) {
                    showLiveCameraOverlay = true
                }

                // Mute button
                voiceControlButton(
                    systemImage: talkStore.isMuted ? "mic.slash.fill" : "mic.fill",
                    tint: talkStore.isMuted ? Design.Colors.danger : Design.Colors.foreground,
                    accessibilityLabel: talkStore.isMuted ? "Unmute" : "Mute"
                ) {
                    Task { await talkStore.toggleMute() }
                }

                Spacer()

                // Close button — signal-orange primary end-session
                voiceControlButton(
                    systemImage: "xmark",
                    background: Design.Brand.accent,
                    tint: Design.Colors.background,
                    accessibilityLabel: "End voice session"
                ) {
                    Task {
                        await talkStore.endSession()
                        router.isVoiceOverlayPresented = false
                    }
                }
            } else {
                Spacer()

                // Close button when not active (e.g. failed to start)
                voiceControlButton(
                    systemImage: "xmark",
                    background: Design.Brand.accent,
                    tint: Design.Colors.background,
                    accessibilityLabel: "Close"
                ) {
                    router.isVoiceOverlayPresented = false
                }
            }
        }
        .padding(.horizontal, Design.Spacing.xl)
    }

    private func voiceControlButton(
        systemImage: String,
        background: Color = Design.Colors.surface,
        tint: Color = Design.Colors.foreground,
        accessibilityLabel: String,
        action: @escaping () -> Void
    ) -> some View {
        Button(action: action) {
            Image(systemName: systemImage)
                .font(.system(size: 18, weight: .medium))
                .foregroundStyle(tint)
                .frame(width: 52, height: 52)
                .background(background)
                .overlay(
                    Circle().stroke(
                        background == Design.Colors.surface
                            ? Design.Colors.border : Color.clear,
                        lineWidth: 1
                    )
                )
                .clipShape(Circle())
        }
        .accessibilityLabel(accessibilityLabel)
    }
}
