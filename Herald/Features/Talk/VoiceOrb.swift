import SwiftUI

/// Herald 2.1 voice orb.
///
/// Default theme: a bone → mist → signal-blue radial, so the orb reads as a lit
/// bone object sitting in cobalt rather than a generic glowing ball. Herald OLED:
/// a darker shell with a brighter signal band and cleaner, thinner rings, so it
/// stays premium against true black instead of blooming.
///
/// Under Reduce Motion the scale breathing is replaced by an opacity shift — the
/// state remains legible without any size animation.
struct VoiceOrb: View {
    let voiceState: VoiceState
    let connectionState: TalkConnectionState

    @State private var pulseScale: CGFloat = 1.0
    @State private var pulseOpacity: Double = 1.0

    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    private var palette: ThemePalette {
        ThemeManager.shared.currentPalette
    }

    /// State-driven ring tint, resolved through the active theme.
    private var ringTint: Color {
        switch voiceState {
        case .speaking: return palette.accentHot
        case .thinking: return palette.accent
        case .listening: return palette.foreground
        default: return palette.tertiaryForeground
        }
    }

    private var isConnected: Bool {
        connectionState == .connected
    }

    /// OLED wants thinner, cleaner rings and no soft halo.
    private var isOLED: Bool {
        palette.prefersSharpEdges
    }

    var body: some View {
        ZStack {
            // Outer breath ring
            Circle()
                .stroke(ringTint.opacity(isOLED ? 0.26 : 0.18), lineWidth: 1)
                .frame(
                    width: Design.Size.voiceOrbSize * 1.45,
                    height: Design.Size.voiceOrbSize * 1.45
                )
                .scaleEffect(pulseScale)
                .opacity(pulseOpacity)

            // Inner breath ring — a fill in the default theme, a hairline in OLED
            // so the black ground stays clean.
            Group {
                if isOLED {
                    Circle().stroke(ringTint.opacity(0.16), lineWidth: 1)
                } else {
                    Circle().fill(ringTint.opacity(0.08))
                }
            }
            .frame(
                width: Design.Size.voiceOrbSize * 1.18,
                height: Design.Size.voiceOrbSize * 1.18
            )
            .scaleEffect(pulseScale * 0.98)
            .opacity(pulseOpacity)

            // Main orb
            Circle()
                .fill(orbGradient)
                .frame(
                    width: Design.Size.voiceOrbSize,
                    height: Design.Size.voiceOrbSize
                )
                .overlay(
                    Circle().stroke(
                        isOLED ? palette.divider : Design.Colors.borderStrong,
                        lineWidth: 1
                    )
                )
                .shadow(
                    color: palette.deepInk.opacity(isOLED ? 0.55 : 0.40),
                    radius: isOLED ? 10 : 18,
                    x: 0,
                    y: isOLED ? 6 : 10
                )
        }
        .onChange(of: voiceState) { updateAnimation() }
        .onChange(of: connectionState) { updateAnimation() }
        .onChange(of: reduceMotion) { updateAnimation() }
        .onAppear { updateAnimation() }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("Voice status: \(voiceState.displayLabel)")
    }

    /// Connected: bone highlight → mist body → signal-blue rim (the 2.1
    /// progression). OLED instead uses a dark shell with a bright signal band.
    /// Disconnected: the orb falls back to inert panel tones.
    private var orbGradient: RadialGradient {
        let colors: [Color]
        if isConnected {
            colors = isOLED
                ? [
                    palette.surfaceRaised,   // dark shell
                    palette.accent,          // signal band
                    palette.accentHot        // bright rim
                  ]
                : [
                    palette.foreground,      // bone highlight
                    palette.secondaryForeground, // mist body
                    palette.accent           // signal-blue rim
                  ]
        } else {
            colors = [
                palette.surfaceRaised,
                palette.panel,
                palette.deepInk
            ]
        }
        return RadialGradient(
            colors: colors,
            center: UnitPoint(x: 0.35, y: 0.3),
            startRadius: 2,
            endRadius: Design.Size.voiceOrbSize * 0.55
        )
    }

    private func updateAnimation() {
        // Reduce Motion: hold scale flat and breathe opacity instead.
        guard !reduceMotion else {
            pulseScale = 1.0
            switch voiceState {
            case .speaking, .thinking:
                withAnimation(Design.Motion.breathe(reduceMotion: true)) {
                    pulseOpacity = 0.55
                }
            default:
                withAnimation(Design.Motion.standard) { pulseOpacity = 1.0 }
            }
            return
        }

        pulseOpacity = 1.0
        switch voiceState {
        case .speaking:
            withAnimation(Design.Motion.breathe) { pulseScale = 1.1 }
        case .thinking:
            withAnimation(Design.Motion.pulse) { pulseScale = 1.05 }
        case .listening:
            // Build 30: the listening orb was static (fell into default).
            // A subtle breath shows the mic is live without competing with
            // speaking/thinking states.  Reduce Motion uses opacity pulse.
            withAnimation(Design.Motion.breathe(reduceMotion: false)) {
                pulseScale = 1.04
            }
        default:
            withAnimation(Design.Motion.gentle) { pulseScale = 1.0 }
        }
    }
}
