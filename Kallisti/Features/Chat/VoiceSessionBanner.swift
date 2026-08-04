import SwiftUI

/// Banner displayed in chat when a voice session's transcript is injected.
struct VoiceSessionBanner: View {
    var duration: TimeInterval?

    var body: some View {
        HStack(spacing: Design.Spacing.xs) {
            dashedLine
            bannerContent
            dashedLine
        }
        .padding(.horizontal, Design.Spacing.md)
        .padding(.vertical, Design.Spacing.sm)
    }

    private var bannerContent: some View {
        HStack(spacing: Design.Spacing.xxs) {
            Image(systemName: "waveform")
                .font(.system(size: 10, weight: .medium))
                .foregroundStyle(Design.Colors.secondaryForeground)

            Text("Voice · Ended")
                .brandEyebrow()

            if let duration {
                Text(formattedDuration(duration))
                    .brandEyebrow(Design.Colors.tertiaryForeground)
            }
        }
    }

    private var dashedLine: some View {
        Rectangle()
            .fill(Design.Colors.divider)
            .frame(height: 1)
    }

    private func formattedDuration(_ seconds: TimeInterval) -> String {
        let minutes = Int(seconds) / 60
        let secs = Int(seconds) % 60
        return String(format: "%d:%02d", minutes, secs)
    }
}
