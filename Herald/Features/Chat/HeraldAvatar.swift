import SwiftUI

struct HeraldAvatar: View {
    var size: CGFloat = Design.Size.avatarSmall

    var body: some View {
        Text("H")
            .font(.system(size: size * 0.42, weight: .regular, design: .monospaced))
            .foregroundStyle(Design.Colors.foreground)
            .frame(width: size, height: size)
            .background(Design.Colors.surface2)
            .overlay(
                Circle().stroke(Design.Colors.border, lineWidth: 1)
            )
            .clipShape(Circle())
            .accessibilityLabel("Herald")
    }
}
