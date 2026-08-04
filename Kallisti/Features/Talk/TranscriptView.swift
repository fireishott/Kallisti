import SwiftUI

struct TranscriptView: View {
    let transcriptItems: [TranscriptItem]
    let voiceState: VoiceState

    var body: some View {
        VStack(spacing: Design.Spacing.xs) {
            if !transcriptItems.isEmpty {
                VStack(spacing: Design.Spacing.sm) {
                    ForEach(Array(transcriptItems.suffix(4))) { item in
                        VStack(spacing: Design.Spacing.xxxs) {
                            Text(item.speaker.displayLabel)
                                .brandEyebrow(Design.Colors.tertiaryForeground)
                            Text("\u{201C}\(item.text)\u{201D}")
                                .font(Design.Typography.editorialItalicSmall)
                                .foregroundStyle(Design.Colors.foreground)
                                .multilineTextAlignment(.center)
                                .opacity(item.isPartial ? 0.72 : 1)
                        }
                        .padding(.horizontal, Design.Spacing.lg)
                    }
                }
                .transition(.opacity.combined(with: .scale(scale: 0.95)))
            }

            HStack(spacing: Design.Spacing.xs) {
                Circle()
                    .fill(voiceState.displayColor)
                    .frame(width: 6, height: 6)
                Text(voiceState.displayLabel)
                    .brandEyebrow(Design.Colors.foreground)
            }
            .animation(Design.Motion.quickResponse, value: voiceState)
        }
        .padding(.horizontal, Design.Spacing.md)
    }
}
