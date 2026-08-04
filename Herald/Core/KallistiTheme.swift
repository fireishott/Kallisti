import SwiftUI

// MARK: - Kallisti Brand Tokens
//
// "To the most beautiful."
//
// Single source of truth for every Kallisti brand color. The identity is derived
// from the app icon: a Greek goddess medallion in platinum and obsidian.
//
// Values come verbatim from the rebrand package (BUILDER_PROMPT.md). Nothing in
// view code should hardcode a hex — reference these tokens, or better, the
// semantic accessors on `Design.Colors`, which resolve through the active theme.

enum KallistiTheme {

    // MARK: - Default: Obsidian (deep charcoal)

    /// The canonical branded dark experience. Controlled contrast — deliberately
    /// *not* pure black, so the obsidian reads as a material rather than a void.
    enum Obsidian {
        /// Deepest ground. Behind everything.
        static let deepInk       = Color(hex: 0x000000)
        /// Default app background.
        static let background    = Color(hex: 0x0C0C10)
        /// Panels and large dark fields.
        static let panel         = Color(hex: 0x16181C)
        /// Cards and controls.
        static let surface       = Color(hex: 0x1C1D22)
        /// Elevated cards.
        static let surfaceRaised = Color(hex: 0x22232A)
        /// Selected surfaces.
        static let selected      = Color(hex: 0x2A2B33)
        /// Primary interactive state (platinum).
        static let accent        = Color(hex: 0xC8CCD2)
        /// Focus, progress, active status.
        static let accentHot     = Color(hex: 0xE5E8EC)

        /// Primary foreground.
        static let bone          = Color(hex: 0xF0F2F5)
        /// Secondary text.
        static let mist          = Color(hex: 0xA8ADB5)
        /// Tertiary text.
        static let pewter        = Color(hex: 0x6B7078)
        /// Borders and rules.
        static let divider       = Color(hex: 0x2A2B33)
    }

    // MARK: - Kallisti OLED (premium black)

    /// The same signal distilled into a darker, sharper presentation. True black
    /// grounds, brighter interactive accents, reduced texture.
    enum OLED {
        static let background    = Color(hex: 0x050507)
        static let surface       = Color(hex: 0x0A0A0E)
        static let surfaceRaised = Color(hex: 0x0F0F14)
        static let accent        = Color(hex: 0xC8CCD2)
        static let accentHot     = Color(hex: 0xE5E8EC)
        static let foreground    = Color(hex: 0xF0F2F5)
        static let secondary     = Color(hex: 0xA8ADB5)
        static let tertiary      = Color(hex: 0x6B7078)
        static let divider       = Color(hex: 0x1C1D22)
    }

    // MARK: - Kallisti Light

    /// Light counterpart to the default Kallisti theme. Clean paper, dark ink.
    enum Light {
        static let background    = Color(hex: 0xFAFAFA)
        static let surface       = Color(hex: 0xFFFFFF)
        /// Elevated cards sit *above* the surface, so they lift toward white.
        static let surfaceRaised = Color(hex: 0xF5F5F5)
        static let foreground    = Color(hex: 0x0C0C10)
        static let secondary     = Color(hex: 0x4A4D55)
        static let tertiary      = Color(hex: 0x8A909A)
        static let accent        = Color(hex: 0x0C0C10)
        static let accentHot     = Color(hex: 0x1C1D22)
        static let divider       = Color(hex: 0xE0E0E0)
    }

    // MARK: - Semantic signals (shared across all Kallisti themes)

    enum Signal {
        static let success = Color(hex: 0x41C98E)
        static let warning = Color(hex: 0xD9AF53)
        static let danger  = Color(hex: 0xCF4D57)
    }

    // MARK: - Relief-print texture

    /// Stipple/relief-print texture opacities. The icon's scanned-print grain is
    /// part of the identity, but it belongs in backgrounds only — never behind
    /// dense reading content.
    enum Texture {
        /// In-app default theme: 4%.
        static let dark:      Double = 0.04
        /// OLED: 0–3%, kept minimal so true black stays true.
        static let oled:      Double = 0.015
        /// Light theme carries the grain slightly stronger to read as paper.
        static let light:     Double = 0.06
        /// Marketing surfaces (15–45%) — not used in-app, documented for parity.
        static let marketing: Double = 0.30
    }

    // MARK: - Brand mark

    enum Mark {
        /// Transparent Kallisti seal, used for onboarding / about brand moments.
        static let seal               = "KallistiSeal"
        /// Full-bleed icon art, used as a low-opacity wallpaper watermark.
        static let iconImage          = "AppIconImage"
        /// Watermark opacity for chat wallpaper / empty states.
        static let watermarkOpacity:  Double = 0.10
        static let watermarkOLED:     Double = 0.06
        static let watermarkLight:    Double = 0.10
    }
}
