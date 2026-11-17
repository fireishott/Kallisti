import SwiftUI

struct MessageReactionPicker: View {
    let onReaction: (String) -> Void
    @Environment(\.dismiss) private var dismiss

    private let reactions = ["👍", "❤️", "😂", "😮", "😢", "🎉"]

    var body: some View {
        VStack(spacing: Design.Spacing.sm) {
            Text("React")
                .font(Design.Typography.caption)
                .foregroundStyle(Design.Colors.secondaryForeground)

            HStack(spacing: Design.Spacing.md) {
                ForEach(reactions, id: \.self) { reaction in
                    Button {
                        onReaction(reaction)
                        dismiss()
                    } label: {
                        Text(reaction)
                            .font(.system(size: 32))
                    }
                    .buttonStyle(.plain)
                }
            }
        }
        .padding(Design.Spacing.lg)
        .background(Design.Colors.surface)
        .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.xl))
        .shadow(color: .black.opacity(0.2), radius: 10, x: 0, y: 5)
    }
}
