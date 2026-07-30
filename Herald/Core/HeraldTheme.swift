import SwiftUI

// MARK: - Herald 2.1 Brand Tokens
//
// "Ancient signal. Modern interface."
//
// Single source of truth for every Herald brand color. The identity is derived
// from the app icon: a cobalt-blue relief print of a herald horn inside a
// classical hall, bone-white stippling, hard radial rays.
//
// Values come verbatim from the rebrand package
// (`palette/herald-palette.json`, BUILDER_THEME_PROMPT.md). Nothing in view code
// should hardcode a hex — reference these tokens, or better, the semantic
// accessors on `Design.Colors`, which resolve through the active theme.
//
// Orange is NOT part of the Herald identity. It was the pre-2.1 accent and has
// been retired everywhere in the Herald presets.

enum HeraldTheme {

    // MARK: - Default: Temple Blue (deep cobalt)

    /// The canonical branded dark experience. Controlled contrast — deliberately
    /// *not* pure black, so the cobalt reads as a material rather than a void.
    enum Cobalt {
        /// Deepest ground. Behind everything.
        static let deepInk = Color(hex: 0x020813)
        /// Default app background.
        static let background = Color(hex: 0x030C1C)
        /// Panels and large dark fields.
        static let templeBlue = Color(hex: 0x071C3D)
        /// Cards and controls.
        static let surface = Color(hex: 0x0C3569)
        /// Elevated cards.
        static let surfaceRaised = Color(hex: 0x123F78)
        /// Selected surfaces.
        static let royalBlue = Color(hex: 0x1A4F97)
        /// Primary interactive state.
        static let signalBlue = Color(hex: 0x306FD6)
        /// Focus, progress, active status.
        static let signalBlueHot = Color(hex: 0x5797F1)

        /// Primary foreground.
        static let bone = Color(hex: 0xF1F5F3)
        /// Secondary text.
        static let mist = Color(hex: 0xBCCDDA)
        /// Tertiary text.
        static let steel = Color(hex: 0x789AB8)
        /// Borders and rules.
        static let divider = Color(hex: 0x466C96)
    }

    // MARK: - Herald OLED (premium black)

    /// The same signal distilled into a darker, sharper presentation. True black
    /// grounds, brighter interactive blues, reduced texture.
    enum OLED {
        static let black = Color(hex: 0x000000)
        static let nearBlack = Color(hex: 0x05070B)
        static let surface = Color(hex: 0x0A1220)
        static let surfaceRaised = Color(hex: 0x0F1C30)
        static let accent = Color(hex: 0x3D7BFF)
        static let accentHot = Color(hex: 0x7AB0FF)
        static let foreground = Color(hex: 0xF5F7FA)
        static let secondary = Color(hex: 0xA8B7CC)
        static let tertiary = Color(hex: 0x6F84A0)
        static let divider = Color(hex: 0x24344D)
    }

    // MARK: - Herald Light

    /// Light counterpart to the default Herald theme. Cool paper, cobalt ink.
    enum Light {
        static let background = Color(hex: 0xE9EEF2)
        static let surface = Color(hex: 0xDCE5EC)
        /// Elevated cards sit *above* the surface, so they lift toward white.
        static let surfaceRaised = Color(hex: 0xF2F6F9)
        static let foreground = Color(hex: 0x061A38)
        static let secondary = Color(hex: 0x355A7D)
        static let tertiary = Color(hex: 0x5B7B99)
        static let accent = Color(hex: 0x1A4F97)
        static let accentHot = Color(hex: 0x306FD6)
        static let divider = Color(hex: 0x789AB8)
    }

    // MARK: - Semantic signals (shared across all Herald themes)

    enum Signal {
        static let success = Color(hex: 0x41C98E)
        static let warning = Color(hex: 0xD9AF53)
        static let danger = Color(hex: 0xCF4D57)
    }

    // MARK: - Relief-print texture

    /// Stipple/relief-print texture opacities. The icon's scanned-print grain is
    /// part of the identity, but it belongs in backgrounds only — never behind
    /// dense reading content.
    enum Texture {
        /// In-app default theme: 3–8%. We sit mid-range.
        static let cobalt: Double = 0.055
        /// OLED: 0–3%, kept minimal so true black stays true.
        static let oled: Double = 0.02
        /// Light theme carries the grain slightly stronger to read as paper.
        static let light: Double = 0.07
        /// Marketing surfaces (15–45%) — not used in-app, documented for parity.
        static let marketing: Double = 0.30
    }

    // MARK: - Brand mark

    enum Mark {
        /// Transparent herald seal, used for onboarding / about brand moments.
        static let seal = "HeraldSeal"
        /// Full-bleed icon art, used as a low-opacity wallpaper watermark.
        static let iconImage = "AppIconImage"
        /// Watermark opacity for chat wallpaper / empty states.
        static let watermarkOpacity: Double = 0.14
        static let watermarkOpacityOLED: Double = 0.07
    }
}

// MARK: - Thread-safe palette snapshot

/// A lock-guarded copy of the active palette, readable from any thread.
///
/// `Design.Colors` / `Design.Brand` are overwhelmingly read from SwiftUI view
/// bodies on the main actor, but they are plain `static var`s with no isolation
/// of their own, so nothing stops a non-isolated caller from reading one.
/// Resolving them through `MainActor.assumeIsolated` makes any such read a hard
/// crash: `assumeIsolated` calls `dispatch_assert_queue`, which traps with
/// `EXC_BREAKPOINT` rather than returning an error.
///
/// This snapshot is written on the main actor whenever the theme changes and read
/// without isolation, so a color lookup can never take the process down. The
/// initial value is the Herald 2.1 default, which is also the launch default —
/// so a read that races ahead of the first sync still gets correct branding.
enum ThemeSnapshot {
    private static let lock = NSLock()
    nonisolated(unsafe) private static var stored: ThemePalette = ThemePreset.herald.darkColors

    /// The active palette. Safe from any thread or actor.
    static var current: ThemePalette {
        lock.lock()
        defer { lock.unlock() }
        return stored
    }

    /// Publish a new active palette. Called by `ThemeManager` on every change.
    static func update(_ palette: ThemePalette) {
        lock.lock()
        stored = palette
        lock.unlock()
    }

    /// The palette that design-token lookups should use.
    ///
    /// On the main thread this reads `ThemeManager` directly, so a SwiftUI view
    /// body evaluating `Design.Colors.background` registers an observation
    /// dependency and re-renders when the theme changes — switching appearance in
    /// Settings has to repaint live. Off the main thread it returns the snapshot
    /// instead of trapping.
    static var activePalette: ThemePalette {
        if Thread.isMainThread {
            return MainActor.assumeIsolated { ThemeManager.shared.currentPalette }
        }
        return current
    }
}
