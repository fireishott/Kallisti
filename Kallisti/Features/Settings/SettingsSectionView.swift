import SwiftUI

struct SettingsSectionView<Content: View>: View {
    let title: String
    @ViewBuilder let content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: Design.Spacing.sm) {
            Text(title)
                .brandEyebrow()
                .padding(.leading, Design.Spacing.xxs)

            VStack(spacing: 0) {
                content
            }
            .padding(Design.Spacing.md)
            .frame(maxWidth: .infinity)
            .background(Design.Colors.surface)
            .overlay(
                RoundedRectangle(cornerRadius: Design.CornerRadius.lg)
                    .stroke(Design.Colors.border, lineWidth: 1)
            )
            .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.lg))
        }
    }
}
