import SwiftUI

// MARK: - Design Tokens
// Public Ethos brand kit — monospace-forward, bone on near-black,
// signal-orange agent accent. All visual constants for HermesMobile.
// No magic numbers in view code.

enum Design {

    // MARK: - Brand

    enum Brand {
        /// Signal-orange — the agent accent. Reserved for the primary CTA.
        static let accent = Color(hex: 0xFF3F00)
        /// Digital-blue (dark-mode variant) — primary interactive.
        static let primary = Color(hex: 0x1B69B1)
        /// Electric digital-blue — press / focus pop.
        static let primaryHot = Color(hex: 0x1F21FF)

        static let accentGradient = LinearGradient(
            colors: [accent, accent.opacity(0.82)],
            startPoint: .topLeading,
            endPoint: .bottomTrailing
        )
    }

    // MARK: - Colors

    enum Colors {
        /// Deep ink — the app's native ground.
        static let background = Color(hex: 0x16181A)
        /// Raised ink — elevated card surface on the dark ground.
        static let backgroundRaised = Color(hex: 0x2D2D29)
        /// Warm bone paper — primary foreground on dark.
        static let foreground = Color(hex: 0xC1C0B6)
        /// Secondary text (grey-300).
        static let secondaryForeground = Color(hex: 0x8D8D85)
        /// Tertiary text (grey-350).
        static let tertiaryForeground = Color(hex: 0x8B8B89)

        /// Low-opacity bone over ground. Cards, bubbles, chips.
        static let surface = Color(hex: 0xC1C0B6, opacity: 0.05)
        static let surface2 = Color(hex: 0xC1C0B6, opacity: 0.08)
        static let surface3 = Color(hex: 0xC1C0B6, opacity: 0.14)

        /// Hair of contrast, never a true outline.
        static let border = Color(hex: 0xC1C0B6, opacity: 0.12)
        static let borderStrong = Color(hex: 0xC1C0B6, opacity: 0.22)
        /// Subtle divider between rows / sections.
        static let divider = Color(hex: 0xC1C0B6, opacity: 0.08)

        /// Semantic signal colors.
        static let success = Color(hex: 0x00C275)
        static let warning = Color(hex: 0xCF9A2F)
        static let danger = Color(hex: 0xCF1322)
        static let violet = Color(hex: 0x7E51B9)

        /// Scrim over content for the voice overlay etc.
        static let scrim = Color(hex: 0x16181A, opacity: 0.72)
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
    // Mono-first. SF Mono stands in for Space Mono on-device; the ASCII-print
    // feeling is the point. Serif italic is an editorial accent, used sparingly
    // for voice-transcript quotes and pullquotes.

    enum Typography {
        /// Giant uppercased display (onboarding title).
        static let heroTitle: Font = .system(size: 40, weight: .regular, design: .monospaced)
        /// Screen-level title (uppercased).
        static let screenTitle: Font = .system(size: 28, weight: .regular, design: .monospaced)
        static let screenTitle2: Font = .system(size: 22, weight: .regular, design: .monospaced)
        /// Section header.
        static let sectionTitle: Font = .system(size: 18, weight: .semibold, design: .monospaced)

        /// UI / body — default mono for chat & copy.
        static let headline: Font = .system(size: 15, weight: .semibold, design: .monospaced)
        static let body: Font = .system(size: 15, weight: .regular, design: .monospaced)
        static let callout: Font = .system(size: 14, weight: .regular, design: .monospaced)
        static let footnote: Font = .system(size: 13, weight: .regular, design: .monospaced)
        static let caption: Font = .system(size: 12, weight: .regular, design: .monospaced)
        static let caption2: Font = .system(size: 11, weight: .regular, design: .monospaced)

        /// Signature brand element — tight, wide-tracked uppercase mono label.
        /// Use via `.brandEyebrow()` for the correct casing + tracking.
        static let eyebrow: Font = .system(size: 11, weight: .regular, design: .monospaced)

        /// Editorial italic pullquote. Stands in for Queens; ships as system serif.
        static let editorialItalic: Font = .system(size: 26, weight: .regular, design: .serif).italic()
        static let editorialItalicSmall: Font = .system(size: 17, weight: .regular, design: .serif).italic()
    }

    // MARK: - Animation

    enum Motion {
        static let quickResponse: Animation = .spring(response: 0.25, dampingFraction: 0.8)
        static let standard: Animation = .spring(response: 0.35, dampingFraction: 0.75)
        static let expressive: Animation = .spring(response: 0.5, dampingFraction: 0.7)
        static let gentle: Animation = .spring(response: 0.6, dampingFraction: 0.85)
        static let pulse: Animation = .easeInOut(duration: 1.2).repeatForever(autoreverses: true)
        static let breathe: Animation = .easeInOut(duration: 2.0).repeatForever(autoreverses: true)
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
