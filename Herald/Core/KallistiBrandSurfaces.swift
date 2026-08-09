import SwiftUI

// MARK: - Kallisti Brand Surfaces
//
// Shared background primitives for the Kallisti identity. Everything here is drawn
// procedurally from theme tokens, so one implementation reads correctly in
// Kallisti (deep obsidian), Kallisti OLED (true black), and Kallisti Light — the
// palette supplies the ground, the glow, the texture weight, and the watermark
// opacity.
//
// Texture rules (BRAND_SYSTEM / BUILDER_PROMPT):
//   * backgrounds only — never behind dense reading content
//   * 4% in-app for the default theme, 1.5% for OLED
//   * the icon is a low-opacity watermark, never a repeated tile
//   * suppressed entirely under Reduce Transparency

/// Relief-print stipple. Stands in for the scanned-print grain of the app icon.
///
/// Dots are placed with a seeded PRNG so the pattern is stable across redraws
/// instead of resampling — and visually flickering — on every `Canvas` pass.
struct KallistiReliefTexture: View {
    /// Base opacity before accessibility adjustment.
    var opacity: Double
    /// Stipple color. Bone by default; the grain reads as ink relief.
    var ink: Color = KallistiTheme.Obsidian.bone
    /// Dot count per full-screen pass.
    var density: Int = 1400
    var seed: UInt64 = 0x4B414C4C // "KALL"

    @Environment(\.accessibilityReduceTransparency) private var reduceTransparency

    var body: some View {
        let resolved = Design.A11y.textureOpacity(opacity, reduceTransparency: reduceTransparency)
        if resolved > 0 {
            Canvas { context, size in
                var generator = KallistiSeededGenerator(seed: seed)
                for _ in 0 ..< density {
                    let x = Double.random(in: 0 ... max(size.width, 1), using: &generator)
                    let y = Double.random(in: 0 ... max(size.height, 1), using: &generator)
                    let radius = Double.random(in: 0.4 ... 1.5, using: &generator)
                    let rect = CGRect(
                        x: x - radius,
                        y: y - radius,
                        width: radius * 2,
                        height: radius * 2
                    )
                    context.fill(Path(ellipseIn: rect), with: .color(ink))
                }
            }
            .opacity(resolved)
            .allowsHitTesting(false)
            .accessibilityHidden(true)
        }
    }
}

/// The signature Kallisti background: deep ink ground, a restrained field
/// rising behind the content, an icon watermark, and relief-print grain.
///
/// Used for the default chat wallpaper, onboarding, and large empty states. In
/// OLED the ground goes true black and the glow/watermark pull back hard, so the
/// same view yields a "premium black" rather than a washed-out dark mode.
struct KallistiBrandField: View {
    /// Show the icon watermark. Off for dense reading surfaces.
    var showsWatermark: Bool = true
    /// Show the relief-print grain.
    var showsTexture: Bool = true
    /// Set when this field sits behind dense reading content (chat, notes). In
    /// OLED the grain is dropped entirely on reading surfaces — the spec allows
    /// 0–3% texture but never behind dense copy in the black theme.
    var isReadingSurface: Bool = false
    /// Vertical placement of the watermark / glow center, in unit space.
    var focus: UnitPoint = UnitPoint(x: 0.5, y: 0.34)

    @Environment(\.accessibilityReduceTransparency) private var reduceTransparency

    private var palette: ThemePalette {
        ThemeManager.shared.currentPalette
    }

    var body: some View {
        GeometryReader { geo in
            let p = palette
            ZStack {
                // Ground — the deepest layer.
                p.deepInk

                // Field. A soft rise rather than a neon glow: the icon's
                // radial rays translated into atmosphere, not decoration.
                RadialGradient(
                    colors: [
                        p.panel.opacity(p.prefersSharpEdges ? 0.55 : 0.95),
                        p.panel.opacity(p.prefersSharpEdges ? 0.18 : 0.45),
                        .clear
                    ],
                    center: focus,
                    startRadius: 0,
                    endRadius: max(geo.size.width, geo.size.height) * 0.85
                )

                // Watermark — the mark itself, well below text contrast.
                if showsWatermark {
                    let markSize = min(geo.size.width * 0.92, 460)
                    Image(KallistiTheme.Mark.iconImage)
                        .resizable()
                        .aspectRatio(contentMode: .fit)
                        .frame(width: markSize, height: markSize)
                        .opacity(p.watermarkOpacity)
                        .position(
                            x: geo.size.width * focus.x,
                            y: geo.size.height * focus.y
                        )
                        .blendMode(.screen)
                        .allowsHitTesting(false)
                        .accessibilityHidden(true)
                }

                // A single accent breath near the focus, kept very low.
                RadialGradient(
                    colors: [p.accent.opacity(p.prefersSharpEdges ? 0.06 : 0.12), .clear],
                    center: focus,
                    startRadius: 0,
                    endRadius: max(geo.size.width, geo.size.height) * 0.5
                )

                if showsTexture, !(isReadingSurface && p.prefersSharpEdges) {
                    KallistiReliefTexture(opacity: p.textureOpacity)
                }
            }
            .ignoresSafeArea()
        }
    }
}

/// Deterministic PRNG (xorshift64) used only to keep stipple patterns stable
/// across redraws. Not for cryptographic use.
struct KallistiSeededGenerator: RandomNumberGenerator {
    private var state: UInt64

    init(seed: UInt64) {
        state = seed &+ 0x9E37_79B9_7F4A_7C15
    }

    mutating func next() -> UInt64 {
        state ^= state << 13
        state ^= state >> 7
        state ^= state << 17
        return state
    }
}

// MARK: - Brand mark

/// The Kallisti seal, for onboarding and About. Transparent PNG so it sits on any
/// theme ground without a plate behind it.
struct KallistiSealMark: View {
    var size: CGFloat = 96
    var opacity: Double = 1.0

    var body: some View {
        Image(KallistiTheme.Mark.seal)
            .resizable()
            .aspectRatio(contentMode: .fit)
            .frame(width: size, height: size)
            .opacity(opacity)
            .accessibilityHidden(true)
    }
}
