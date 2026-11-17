import SwiftUI
import UIKit

// MARK: - Design Tokens
// Herald 2.1 brand kit — "Ancient signal. Modern interface."
// Cobalt ground, bone foreground, signal-blue interactive. Editorial serif for
// display, mono for status/code/metadata, system sans for body copy.
// All visual constants for Herald. No magic numbers in view code.
//
// Raw hex values live in `KallistiTheme`; per-theme resolution lives in
// `ThemePalette`. This file is the semantic layer view code should consume.

enum Design {

    // MARK: - Brand

    /// Brand-level interactive colors. These resolve through the *active theme*
    /// so a single accent reference recolors correctly in Herald, Herald OLED,
    /// and Herald Light. Pre-2.1 presets keep their own accents.
    enum Brand {
        /// Read from the lock-guarded snapshot, not `MainActor.assumeIsolated` —
        /// these are non-isolated `static var`s and a non-main read would trap.
        private static var palette: ThemePalette { ThemeSnapshot.activePalette }

        /// Primary interactive / agent accent. Signal blue under Herald 2.1.
        ///
        /// This was signal-orange before 2.1. Orange is not part of the Herald
        /// identity and is retired — do not reintroduce it.
        static var accent: Color { palette.accent }
        /// Primary interactive, slightly deeper than `accent`.
        static var primary: Color { palette.accent }
        /// Press / focus pop — the brighter signal.
        static var primaryHot: Color { palette.accentHot }

        static var accentGradient: LinearGradient {
            LinearGradient(
                colors: [accent, accent.opacity(0.82)],
                startPoint: .topLeading,
                endPoint: .bottomTrailing
            )
        }
    }

    // MARK: - Colors

    enum Colors {
        /// See `ThemeSnapshot`: resolving through `MainActor.assumeIsolated` made
        /// every off-main color read an `EXC_BREAKPOINT` crash.
        private static var palette: ThemePalette { ThemeSnapshot.activePalette }

        /// Deep ink — the app's native ground.
        static var background: Color { palette.background }
        /// Raised ink — elevated card surface on the dark ground.
        static var backgroundRaised: Color { palette.surfaceRaised }
        /// The deepest layer, behind `background`. Gutters, scrims, wallpaper base.
        static var deepInk: Color { palette.deepInk }
        /// Large dark fields and panels (Herald: temple blue).
        static var panel: Color { palette.panel }
        /// Warm bone paper — primary foreground on dark.
        static var foreground: Color { palette.foreground }
        /// Secondary text.
        static var secondaryForeground: Color { palette.secondaryForeground }
        /// Tertiary text.
        static var tertiaryForeground: Color { palette.tertiaryForeground }

        /// Layered surfaces. Cards, bubbles, chips.
        ///
        /// These were previously `surface.opacity(1.6)` / `.opacity(2.8)`, which
        /// silently clamped to 1.0 — so all three levels rendered identically
        /// once the palette used opaque colors. They now map to real tokens.
        static var surface: Color { palette.surface }
        static var surface2: Color { palette.surfaceRaised }
        static var surface3: Color { palette.surfaceSelected }

        /// Selected / active surface fill.
        static var surfaceSelected: Color { palette.surfaceSelected }

        /// Interactive accents, theme-resolved.
        static var accent: Color { palette.accent }
        static var accentHot: Color { palette.accentHot }

        /// Hair of contrast, never a true outline. Opacities stay <= 1 so the
        /// hairline actually reads as a hairline.
        static var border: Color { palette.divider.opacity(0.55) }
        static var borderStrong: Color { palette.divider.opacity(0.85) }
        /// Subtle divider between rows / sections.
        static var divider: Color { palette.divider.opacity(0.40) }

        /// Semantic signal colors, theme-resolved.
        static var success: Color { palette.success }
        static var warning: Color { palette.warning }
        static var danger: Color { palette.danger }
        static let violet = Color(hex: 0x7E51B9)

        /// Relief-print texture weight for backgrounds. 0 disables texture.
        static var textureOpacity: Double { palette.textureOpacity }
        /// Icon watermark opacity for wallpapers and empty states.
        static var watermarkOpacity: Double { palette.watermarkOpacity }
        /// OLED-style presentation: sharper edges, lower halo.
        static var prefersSharpEdges: Bool { palette.prefersSharpEdges }

        /// Scrim over content for the voice overlay etc.
        static var scrim: Color { palette.deepInk.opacity(0.78) }
    }

    // MARK: - Spacing (4pt base grid)

    enum Spacing {
        static let xxxs: CGFloat = 2
        static let xxs: CGFloat = 4
        static let xs: CGFloat = 8
        static let sm: CGFloat = 12
        static let md: CGFloat = 16
        static let lg: CGFloat = 24
        static let xl: CGFloat = 32
        static let xxl: CGFloat = 48
        static let xxxl: CGFloat = 64
    }

    // MARK: - Corner Radii

    enum CornerRadius {
        static let xs: CGFloat = 4
        static let sm: CGFloat = 8
        static let md: CGFloat = 12
        static let lg: CGFloat = 16
        static let xl: CGFloat = 20
        /// Message bubbles and cards — generous, mobile-scale.
        static let xxl: CGFloat = 24
        /// Pill — composer, capsules, action buttons.
        static let pill: CGFloat = 999
        static let full: CGFloat = .infinity
    }

    // MARK: - Typography
    //
    // Herald 2.1 uses three type roles with clear separation. Do not blur them:
    //
    //   1. DISPLAY  — system serif (New York). Hero and screen titles, brand
    //                 moments, editorial headings. Monumental, not decorative.
    //   2. MONO     — status, chips, model names, code, metadata, eyebrows.
    //                 The technical register.
    //   3. BODY     — system default (SF Pro). All long-form and settings copy.
    //
    // Long paragraphs are never monospaced and never all-caps. Before 2.1 this
    // file was mono-first for everything, which cost readability in chat and
    // settings; body roles are now system sans on purpose.
    //
    // All sizes use `.system(size:)`, which participates in Dynamic Type scaling
    // via `.dynamicTypeSize` / relative text styles at the call site.

    enum Typography {

        // MARK: Display (serif)

        /// Giant display (onboarding / brand title).
        static let heroTitle: Font = .system(size: 40, weight: .regular, design: .serif)
        /// Screen-level title.
        static let screenTitle: Font = .system(size: 28, weight: .regular, design: .serif)
        static let screenTitle2: Font = .system(size: 22, weight: .regular, design: .serif)
        /// Section / card heading — editorial serif.
        static let sectionTitle: Font = .system(size: 18, weight: .semibold, design: .serif)
        /// Largest brand moment, e.g. the wordmark in About.
        static let display: Font = .system(size: 52, weight: .medium, design: .serif)

        // MARK: Body (system sans)

        static let headline: Font = .system(size: 15, weight: .semibold)
        static let body: Font = .system(size: 15, weight: .regular)
        static let callout: Font = .system(size: 14, weight: .regular)
        static let footnote: Font = .system(size: 13, weight: .regular)

        /// Small explanatory copy under fields and controls.
        ///
        /// Distinct from `caption`: helper text is prose, sometimes several
        /// sentences, so it must not be monospaced. `caption` stays mono for
        /// genuine metadata (timestamps, token counts, model ids).
        static let helper: Font = .system(size: 12, weight: .regular)

        // MARK: Mono (status / code / metadata)

        /// Metadata and small technical labels.
        static let caption: Font = .system(size: 12, weight: .regular, design: .monospaced)
        static let caption2: Font = .system(size: 11, weight: .regular, design: .monospaced)
        /// Inline and block code.
        static let code: Font = .system(size: 13, weight: .regular, design: .monospaced)
        static let codeSmall: Font = .system(size: 12, weight: .regular, design: .monospaced)
        /// Status pills, relay state, connection indicators.
        static let status: Font = .system(size: 12, weight: .medium, design: .monospaced)
        /// Model / profile / context chips.
        static let chip: Font = .system(size: 12, weight: .regular, design: .monospaced)

        /// Signature brand element — tight, wide-tracked uppercase mono label.
        /// Use via `.brandEyebrow()` for the correct casing + tracking.
        static let eyebrow: Font = .system(size: 11, weight: .regular, design: .monospaced)

        /// Editorial italic pullquote.
        static let editorialItalic: Font = .system(size: 26, weight: .regular, design: .serif).italic()
        static let editorialItalicSmall: Font = .system(size: 17, weight: .regular, design: .serif).italic()
    }

    // MARK: - Animation
    //
    // Herald 2.1 timing bands:
    //   fast controls        0.20–0.28 s
    //   standard transitions 0.30–0.40 s
    //   voice breathing      1.8–2.4 s
    // No shimmer spam. Under Reduce Motion, scale-breathing is replaced by
    // opacity/state shifts — see `breathe(reduceMotion:)`.

    enum Motion {
        /// Fast controls — taps, toggles, chip state.
        static let quickResponse: Animation = .spring(response: 0.25, dampingFraction: 0.8)
        /// Standard transitions — sheets, screen changes, list inserts.
        static let standard: Animation = .spring(response: 0.35, dampingFraction: 0.75)
        static let expressive: Animation = .spring(response: 0.5, dampingFraction: 0.7)
        static let gentle: Animation = .spring(response: 0.6, dampingFraction: 0.85)
        static let pulse: Animation = .easeInOut(duration: 1.2).repeatForever(autoreverses: true)

        /// Voice breathing period, mid-band.
        static let breatheDuration: Double = 2.1
        static let breathe: Animation = .easeInOut(duration: breatheDuration).repeatForever(autoreverses: true)

        /// Reduce-Motion-aware breathing. When Reduce Motion is on, callers
        /// should drive opacity instead of scale; the animation itself stays
        /// gentle rather than being removed entirely so status remains legible.
        static func breathe(reduceMotion: Bool) -> Animation {
            reduceMotion
                ? .easeInOut(duration: breatheDuration).repeatForever(autoreverses: true)
                : breathe
        }

        /// Scale amplitude for breathing effects — flattened under Reduce Motion.
        static func breatheScale(reduceMotion: Bool) -> CGFloat {
            reduceMotion ? 1.0 : 1.06
        }
    }

    // MARK: - Accessibility

    enum A11y {
        /// Card/chip fill opacity bump when Reduce Transparency is enabled, so
        /// translucent surfaces become solid enough to read against texture.
        static func surfaceOpacity(reduceTransparency: Bool) -> Double {
            reduceTransparency ? 1.0 : 0.92
        }

        /// Texture is suppressed entirely under Reduce Transparency — the grain
        /// is decorative and competes with text.
        static func textureOpacity(_ base: Double, reduceTransparency: Bool) -> Double {
            reduceTransparency ? 0 : base
        }
    }

    // MARK: - Size

    enum Size {
        static let minTapTarget: CGFloat = 44
        static let iconTiny: CGFloat = 10
        static let iconSmall: CGFloat = 16
        static let iconMedium: CGFloat = 24
        static let iconLarge: CGFloat = 32
        static let iconXL: CGFloat = 40
        static let iconHero: CGFloat = 60
        static let avatarSmall: CGFloat = 32
        static let avatarMedium: CGFloat = 48
        static let avatarLarge: CGFloat = 80
        static let thumbnailSmall: CGFloat = 64
        static let thumbnailMedium: CGFloat = 120
        static let thumbnailLarge: CGFloat = 200
        static let heroHeight: CGFloat = 300
        static let cardMinHeight: CGFloat = 160
        static let badgeSize: CGFloat = 22
        static let inputBarHeight: CGFloat = 52
        static let voiceOrbSize: CGFloat = 160
        static let glassCircleButton: CGFloat = 40
    }
}

// MARK: - Color Hex Extension

extension Color {
    init(hex: UInt, opacity: Double = 1.0) {
        self.init(
            red: Double((hex >> 16) & 0xFF) / 255.0,
            green: Double((hex >> 8) & 0xFF) / 255.0,
            blue: Double(hex & 0xFF) / 255.0,
            opacity: opacity
        )
    }

    /// Best-effort 6-digit RGB hex string (no `#`) for persistence.
    /// Returns nil for dynamic/system/accent colors that have no concrete
    /// RGB components, so a picker result always round-trips cleanly.
    var hexString: String? {
        let uiColor = UIColor(self)
        var r: CGFloat = 0, g: CGFloat = 0, b: CGFloat = 0, a: CGFloat = 0
        guard uiColor.getRed(&r, green: &g, blue: &b, alpha: &a) else { return nil }
        return String(
            format: "%02X%02X%02X",
            Int(round(r * 255)), Int(round(g * 255)), Int(round(b * 255))
        )
    }
}

// MARK: - Brand Typography Modifiers

extension View {
    /// Signature uppercase-mono label: `CONTEXT WINDOW`, `VOICE MODE`, `ALT 1`.
    /// Tight size, wide tracking, uppercased, muted foreground.
    func brandEyebrow(_ color: Color? = nil) -> some View {
        self
            .font(Design.Typography.eyebrow)
            .textCase(.uppercase)
            .tracking(1.2)
            .foregroundStyle(color ?? Design.Colors.secondaryForeground)
    }
}
