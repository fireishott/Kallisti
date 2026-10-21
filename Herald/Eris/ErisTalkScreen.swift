import SwiftUI

// MARK: - Talk model

/// The voice lane's readiness probe over `GET /v1/talk/readiness`.
@MainActor
@Observable
final class ErisTalkModel {
    private(set) var readiness: ErisTalkReadiness?
    private(set) var hasLoaded = false

    private let client: ErisClient

    init(client: ErisClient) {
        self.client = client
    }

    func loadIfNeeded() async {
        guard !hasLoaded else { return }
        await load()
    }

    func load() async {
        readiness = try? await client.talkReadiness()
        hasLoaded = true
    }
}

// MARK: - Talk screen

/// The voice lane. Present with the full identity, but honest: the backend
/// readiness probe reports `ready: false` (in development) until the route is
/// real, so this is not wired to a fake no-op.
struct ErisTalkScreen: View {
    let model: ErisTalkModel

    var body: some View {
        VStack(spacing: Design.Spacing.lg) {
            Image(systemName: "waveform")
                .font(.system(size: 44, weight: .regular))
                .foregroundStyle(Design.Colors.secondaryForeground)
            Text("Talk")
                .font(Design.Typography.screenTitle2)
                .foregroundStyle(Design.Colors.foreground)
            Text("Talk is the live voice conversation lane. It is still in development on the Eris backend.")
                .font(Design.Typography.body)
                .foregroundStyle(Design.Colors.secondaryForeground)
                .multilineTextAlignment(.center)
                .fixedSize(horizontal: false, vertical: true)
            Label(
                model.readiness?.reason ?? "in_development",
                systemImage: model.readiness?.ready == true ? "checkmark.circle" : "hourglass"
            )
            .font(Design.Typography.caption)
            .foregroundStyle(Design.Colors.tertiaryForeground)
            .padding(.horizontal, Design.Spacing.md)
            .padding(.vertical, Design.Spacing.xs)
            .background(Design.Colors.backgroundRaised)
            .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.md))
        }
        .padding(Design.Spacing.xl)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Design.Colors.background.ignoresSafeArea())
        .navigationTitle("Talk")
        .navigationBarTitleDisplayMode(.inline)
        .task { await model.loadIfNeeded() }
    }
}
