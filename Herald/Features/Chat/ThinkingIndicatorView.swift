import SwiftUI

struct ThinkingIndicatorView: View {
    let startTime: Date
    var toolActivity: String? = nil

    @State private var showDetail = false
    @State private var isPulsing = false

    var body: some View {
        HStack(alignment: .top, spacing: Design.Spacing.xs) {
            HeraldAvatar(size: Design.Size.avatarSmall)
                .opacity(isPulsing ? 0.5 : 1.0)

            VStack(alignment: .leading, spacing: Design.Spacing.xxs) {
                if let activity = toolActivity {
                    toolActivityLabel(activity)
                } else {
                    thinkingDots
                }
                // Always show elapsed time while thinking
                TimelineView(.periodic(from: startTime, by: 1)) { context in
                    let elapsed = context.date.timeIntervalSince(startTime)
                    Text("Thinking… \(formatElapsed(elapsed))")
                        .brandEyebrow(Design.Colors.tertiaryForeground)
                }
            }

            Spacer(minLength: Design.Spacing.xxl)
        }
        .padding(.horizontal, Design.Spacing.md)
        .contentShape(Rectangle())
        .onAppear {
            withAnimation(Design.Motion.breathe) {
                isPulsing = true
            }
        }
    }

    private func toolActivityLabel(_ label: String) -> some View {
        Text(label)
            .brandEyebrow()
            .padding(.horizontal, Design.Spacing.sm)
            .padding(.vertical, Design.Spacing.xxs)
            .background(Design.Colors.surface)
            .overlay(Capsule().stroke(Design.Colors.border, lineWidth: 1))
            .clipShape(Capsule())
            .transition(.opacity)
            .animation(Design.Motion.quickResponse, value: label)
    }

    private var thinkingDots: some View {
        HStack(spacing: Design.Spacing.xxs) {
            ForEach(0 ..< 3, id: \.self) { index in
                Circle()
                    .fill(Design.Colors.secondaryForeground)
                    .frame(width: 6, height: 6)
                    .opacity(isPulsing ? 0.3 : 0.8)
                    .animation(
                        .easeInOut(duration: 0.6)
                            .repeatForever(autoreverses: true)
                            .delay(Double(index) * 0.2),
                        value: isPulsing
                    )
            }
        }
        .padding(.vertical, Design.Spacing.sm)
    }

    private func formatElapsed(_ interval: TimeInterval) -> String {
        let seconds = Int(interval)
        if seconds < 60 {
            return "\(seconds)s"
        }
        return "\(seconds / 60)m \(seconds % 60)s"
    }
}
