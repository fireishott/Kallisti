import SwiftUI

struct CaptureScreen: View {
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        ZStack {
            Design.Colors.background
                .ignoresSafeArea()

            VStack(spacing: Design.Spacing.md) {
                Text("Capture · Soon")
                    .brandEyebrow()

                Text("visual capture")
                    .font(Design.Typography.editorialItalic)
                    .foregroundStyle(Design.Colors.foreground)

                Text("Camera and canvas features are in development. This surface will host future Herald visual capabilities.")
                    .font(Design.Typography.body)
                    .foregroundStyle(Design.Colors.secondaryForeground)
                    .multilineTextAlignment(.center)
                    .padding(.horizontal, Design.Spacing.xl)

                Button {
                    dismiss()
                } label: {
                    Text("Go Back")
                        .brandEyebrow(Design.Colors.background)
                        .padding(.horizontal, Design.Spacing.lg)
                        .padding(.vertical, Design.Spacing.md)
                }
                .background(Design.Brand.accent)
                .clipShape(Capsule())
                .padding(.top, Design.Spacing.sm)
            }
        }
        .navigationTitle("Capture")
    }
}
