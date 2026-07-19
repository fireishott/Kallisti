import SwiftUI

struct StatusCardView: View {
    let connectionLabel: String
    let messageCount: Int
    let conversationID: UUID?
    let tokenUsage: TokenUsage?
    let dismissAction: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: Design.Spacing.sm) {
            HStack(spacing: Design.Spacing.xs) {
                Text("Session · Status")
                    .brandEyebrow()
                Spacer()
                Button(action: dismissAction) {
                    Image(systemName: "xmark")
                        .font(.system(size: 11, weight: .medium))
                        .foregroundStyle(Design.Colors.secondaryForeground)
                }
            }

            Divider()
                .overlay(Design.Colors.divider)

            statusRow("Connection", value: connectionLabel)
            statusRow("Messages", value: "\(messageCount)")
            if let id = conversationID {
                statusRow("Session", value: String(id.uuidString.prefix(8)))
            }

            if let usage = tokenUsage {
                Divider()
                    .overlay(Design.Colors.divider)
                statusRow("Context", value: "\(usage.promptTokens) tok")
                statusRow("Completion", value: "\(usage.completionTokens)")
                statusRow("Total", value: "\(usage.totalTokens)")
            }
        }
        .padding(Design.Spacing.md)
        .background(Design.Colors.surface)
        .overlay(
            RoundedRectangle(cornerRadius: Design.CornerRadius.lg)
                .stroke(Design.Colors.border, lineWidth: 1)
        )
        .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.lg))
        .padding(.horizontal, Design.Spacing.md)
    }

    private func statusRow(_ label: String, value: String) -> some View {
        HStack {
            Text(label)
                .brandEyebrow()
            Spacer()
            Text(value)
                .font(Design.Typography.callout)
                .foregroundStyle(Design.Colors.foreground)
        }
    }
}
