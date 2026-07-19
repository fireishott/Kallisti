import SwiftUI

/// Bone-toned radial orb per the Public Ethos brand kit.
/// Flat neutral palette — warm highlight over bone body, very subtle breathe ring
/// with a slight color shift depending on state (listening / thinking / speaking).
struct VoiceOrb: View {
    let voiceState: VoiceState
    let connectionState: TalkConnectionState

    @State private var pulseScale: CGFloat = 1.0

    /// State-driven ring tint. Stays in the neutral/brand palette.
    private var ringTint: Color {
        switch voiceState {
        case .speaking: return Design.Brand.accent
        case .thinking: return Design.Brand.primary
        case .listening: return Design.Colors.foreground
        default: return Design.Colors.tertiaryForeground
        }
    }

    private var isConnected: Bool {
        connectionState == .connected
    }

    var body: some View {
        ZStack {
            // Outer breath ring
            Circle()
                .stroke(ringTint.opacity(0.18), lineWidth: 1)
                .frame(
                    width: Design.Size.voiceOrbSize * 1.45,
                    height: Design.Size.voiceOrbSize * 1.45
                )
                .scaleEffect(pulseScale)

            // Inner breath ring
            Circle()
                .fill(ringTint.opacity(0.08))
                .frame(
                    width: Design.Size.voiceOrbSize * 1.18,
                    height: Design.Size.voiceOrbSize * 1.18
                )
                .scaleEffect(pulseScale * 0.98)

            // Main orb — warm bone radial on paper highlight
            Circle()
                .fill(orbGradient)
                .frame(
                    width: Design.Size.voiceOrbSize,
                    height: Design.Size.voiceOrbSize
                )
                .overlay(
                    Circle().stroke(Design.Colors.borderStrong, lineWidth: 1)
                )
                .shadow(color: Color.black.opacity(0.4), radius: 18, x: 0, y: 10)
        }
        .onChange(of: voiceState) { updateAnimation() }
        .onChange(of: connectionState) { updateAnimation() }
        .onAppear { updateAnimation() }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("Voice status: \(voiceState.displayLabel)")
    }

    /// Paper highlight → bone body → ink rim. All in the brand neutral palette.
    private var orbGradient: RadialGradient {
        RadialGradient(
            colors: isConnected
                ? [
                    Color(hex: 0xE8E7DE),   // eggshell highlight
                    Color(hex: 0xC1C0B6),   // bone body
                    Color(hex: 0x8D8D85)    // warm grey rim
                  ]
                : [
                    Color(hex: 0x40443F),   // muted raised
                    Color(hex: 0x2D2D29),   // ink raised
                    Color(hex: 0x16181A)    // deep ink
                  ],
            center: UnitPoint(x: 0.35, y: 0.3),
            startRadius: 2,
            endRadius: Design.Size.voiceOrbSize * 0.55
        )
    }

    private func updateAnimation() {
        switch voiceState {
        case .speaking:
            withAnimation(Design.Motion.breathe) { pulseScale = 1.1 }
        case .thinking:
            withAnimation(Design.Motion.pulse) { pulseScale = 1.05 }
        default:
            withAnimation(Design.Motion.gentle) { pulseScale = 1.0 }
        }
    }
}
