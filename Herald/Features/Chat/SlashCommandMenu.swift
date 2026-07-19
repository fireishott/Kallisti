import SwiftUI

struct SlashCommandMenu: View {
    let commands: [SlashCommand]
    let onSelect: (SlashCommand) -> Void

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 0) {
                ForEach(Array(commands.enumerated()), id: \.element.id) { index, command in
                    if index > 0 {
                        Rectangle()
                            .fill(Design.Colors.divider)
                            .frame(height: 1)
                    }

                    Button { onSelect(command) } label: {
                        HStack(alignment: .firstTextBaseline, spacing: Design.Spacing.sm) {
                            Text(command.displayTitle)
                                .font(Design.Typography.body)
                                .foregroundStyle(Design.Colors.foreground)
                                .frame(width: 100, alignment: .leading)

                            Text(command.description)
                                .brandEyebrow()
                                .lineLimit(1)

                            Spacer(minLength: 0)
                        }
                        .padding(.vertical, Design.Spacing.sm)
                        .padding(.horizontal, Design.Spacing.md)
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                }
            }
        }
        .scrollBounceBehavior(.basedOnSize)
        .frame(maxHeight: 260)
        .background(Design.Colors.backgroundRaised)
        .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.lg))
        .overlay(
            RoundedRectangle(cornerRadius: Design.CornerRadius.lg)
                .stroke(Design.Colors.border, lineWidth: 1)
        )
        .padding(.horizontal, Design.Spacing.md)
    }
}
