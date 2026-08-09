import SwiftUI
import Testing
import UIKit
@testable import Kallisti

/// Kallisti rebrand contract tests.
///
/// These lock the design tokens to the values in the rebrand package
/// (BUILDER_PROMPT.md) so a future edit can't silently drift the brand.
@Suite
struct KallistiThemeTests {

    // MARK: - Helpers

    /// Resolve a SwiftUI `Color` to an 0xRRGGBB integer for comparison against
    /// the published brand hex values.
    private func hex(_ color: Color) -> UInt {
        var r: CGFloat = 0, g: CGFloat = 0, b: CGFloat = 0, a: CGFloat = 0
        UIColor(color).getRed(&r, green: &g, blue: &b, alpha: &a)
        let ri = UInt((r * 255).rounded())
        let gi = UInt((g * 255).rounded())
        let bi = UInt((b * 255).rounded())
        return (ri << 16) | (gi << 8) | bi
    }

    private func alpha(_ color: Color) -> CGFloat {
        var r: CGFloat = 0, g: CGFloat = 0, b: CGFloat = 0, a: CGFloat = 0
        UIColor(color).getRed(&r, green: &g, blue: &b, alpha: &a)
        return a
    }

    /// WCAG relative luminance, used for contrast assertions.
    private func luminance(_ color: Color) -> Double {
        var r: CGFloat = 0, g: CGFloat = 0, b: CGFloat = 0, a: CGFloat = 0
        UIColor(color).getRed(&r, green: &g, blue: &b, alpha: &a)
        func channel(_ c: CGFloat) -> Double {
            let v = Double(c)
            return v <= 0.03928 ? v / 12.92 : pow((v + 0.055) / 1.055, 2.4)
        }
        return 0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b)
    }

    private func contrastRatio(_ a: Color, _ b: Color) -> Double {
        let la = luminance(a), lb = luminance(b)
        let hi = max(la, lb), lo = min(la, lb)
        return (hi + 0.05) / (lo + 0.05)
    }

    // MARK: - Token fidelity

    @Test("Kallisti obsidian tokens match the published brand palette")
    func obsidianTokens() {
        #expect(hex(KallistiTheme.Obsidian.deepInk) == 0x000000)
        #expect(hex(KallistiTheme.Obsidian.background) == 0x0C0C10)
        #expect(hex(KallistiTheme.Obsidian.panel) == 0x16181C)
        #expect(hex(KallistiTheme.Obsidian.surface) == 0x1C1D22)
        #expect(hex(KallistiTheme.Obsidian.surfaceRaised) == 0x22232A)
        #expect(hex(KallistiTheme.Obsidian.selected) == 0x2A2B33)
        #expect(hex(KallistiTheme.Obsidian.accent) == 0xC8CCD2)
        #expect(hex(KallistiTheme.Obsidian.accentHot) == 0xE5E8EC)
        #expect(hex(KallistiTheme.Obsidian.bone) == 0xF0F2F5)
        #expect(hex(KallistiTheme.Obsidian.mist) == 0xA8ADB5)
        #expect(hex(KallistiTheme.Obsidian.pewter) == 0x6B7078)
        #expect(hex(KallistiTheme.Obsidian.divider) == 0x2A2B33)
    }

    @Test("Kallisti OLED tokens match the published brand palette")
    func oledTokens() {
        #expect(hex(KallistiTheme.OLED.background) == 0x050507)
        #expect(hex(KallistiTheme.OLED.surface) == 0x0A0A0E)
        #expect(hex(KallistiTheme.OLED.surfaceRaised) == 0x0F0F14)
        #expect(hex(KallistiTheme.OLED.accent) == 0xC8CCD2)
        #expect(hex(KallistiTheme.OLED.accentHot) == 0xE5E8EC)
        #expect(hex(KallistiTheme.OLED.foreground) == 0xF0F2F5)
        #expect(hex(KallistiTheme.OLED.secondary) == 0xA8ADB5)
        #expect(hex(KallistiTheme.OLED.tertiary) == 0x6B7078)
        #expect(hex(KallistiTheme.OLED.divider) == 0x1C1D22)
    }

    @Test("Kallisti Light tokens match the published brand palette")
    func lightTokens() {
        #expect(hex(KallistiTheme.Light.background) == 0xFAFAFA)
        #expect(hex(KallistiTheme.Light.surface) == 0xFFFFFF)
        #expect(hex(KallistiTheme.Light.foreground) == 0x0C0C10)
        #expect(hex(KallistiTheme.Light.secondary) == 0x4A4D55)
        #expect(hex(KallistiTheme.Light.tertiary) == 0x8A909A)
        #expect(hex(KallistiTheme.Light.accent) == 0x0C0C10)
        #expect(hex(KallistiTheme.Light.accentHot) == 0x1C1D22)
        #expect(hex(KallistiTheme.Light.divider) == 0xE0E0E0)
    }

    @Test("Semantic signal colors match the published brand palette")
    func signalTokens() {
        #expect(hex(KallistiTheme.Signal.success) == 0x41C98E)
        #expect(hex(KallistiTheme.Signal.warning) == 0xD9AF53)
        #expect(hex(KallistiTheme.Signal.danger) == 0xCF4D57)
    }

    // MARK: - Preset wiring

    @Test("Kallisti default dark palette is wired to the obsidian tokens")
    func kallistiDarkPalette() {
        let p = ThemePreset.kallisti.darkColors
        #expect(hex(p.background) == 0x0C0C10)
        #expect(hex(p.deepInk) == 0x000000)
        #expect(hex(p.panel) == 0x16181C)
        #expect(hex(p.surface) == 0x1C1D22)
        #expect(hex(p.surfaceRaised) == 0x22232A)
        #expect(hex(p.surfaceSelected) == 0x2A2B33)
        #expect(hex(p.foreground) == 0xF0F2F5)
        #expect(hex(p.secondaryForeground) == 0xA8ADB5)
        #expect(hex(p.tertiaryForeground) == 0x6B7078)
        #expect(hex(p.accent) == 0xC8CCD2)
        #expect(hex(p.accentHot) == 0xE5E8EC)
        #expect(p.prefersSharpEdges == false)
    }

    @Test("Kallisti OLED palette is wired to the OLED tokens, with true-black deep ink")
    func kallistiOLEDPalette() {
        let p = ThemePreset.kallistiOLED.darkColors
        #expect(hex(p.deepInk) == 0x000000, "OLED deepest layer must be true black")
        #expect(hex(p.background) == 0x050507)
        #expect(hex(p.surface) == 0x0A0A0E)
        #expect(hex(p.surfaceRaised) == 0x0F0F14)
        #expect(hex(p.accent) == 0xC8CCD2)
        #expect(hex(p.accentHot) == 0xE5E8EC)
        #expect(p.prefersSharpEdges == true, "OLED presentation uses sharper edges")
    }

    @Test("Kallisti light palette is wired to the light tokens")
    func kallistiLightPalette() {
        let p = ThemePreset.kallisti.lightColors
        #expect(hex(p.background) == 0xFAFAFA)
        #expect(hex(p.surface) == 0xFFFFFF)
        #expect(hex(p.foreground) == 0x0C0C10)
        #expect(hex(p.accent) == 0x0C0C10)
    }

    @Test("Kallisti OLED resolves to the shared light counterpart in light mode")
    func oledLightFallsBackToKallistiLight() {
        #expect(hex(ThemePreset.kallistiOLED.lightColors.background) == 0xFAFAFA)
    }

    // MARK: - Appearance mapping

    @Test("Every Kallisti appearance round-trips through its stored axes")
    func appearanceRoundTrip() {
        for appearance in KallistiAppearance.allCases {
            let resolved = KallistiAppearance.resolve(
                preset: appearance.preset,
                colorScheme: appearance.colorScheme
            )
            #expect(resolved == appearance,
                    "\(appearance.rawValue) did not round-trip")
        }
    }

    @Test("Appearance list is exactly System, Kallisti, Kallisti OLED, Kallisti Light")
    func appearanceRoster() {
        #expect(KallistiAppearance.allCases.map(\.label) ==
                ["System", "Kallisti", "Kallisti OLED", "Kallisti Light"])
    }

    @Test("A pre-2.1 preset resolves to no Kallisti appearance")
    func legacyPresetHasNoAppearance() {
        #expect(KallistiAppearance.resolve(preset: .slate, colorScheme: .dark) == nil)
        #expect(KallistiAppearance.resolve(preset: .cyberpunk, colorScheme: .light) == nil)
    }

    @Test("Pre-2.1 presets are retained as secondary options")
    func legacyPresetsRetained() {
        #expect(ThemePreset.legacyPresets == [.midnight, .ember, .mono, .cyberpunk, .slate])
        // All presets remain selectable/decodable — no case was removed.
        for raw in ["midnight", "ember", "mono", "cyberpunk", "slate", "kallisti"] {
            #expect(ThemePreset(rawValue: raw) != nil)
        }
    }

    // MARK: - Persistence

    @Test("themePreset default is Kallisti and legacy 'nous'/'herald' still migrate")
    func presetPersistence() throws {
        // Absent key -> Kallisti default.
        let empty = try JSONDecoder().decode(UserSettings.self, from: Data("{}".utf8))
        #expect(empty.themePreset == .kallisti)

        // The 1.0.0 rename migration must survive the rebrand.
        let legacy = try JSONDecoder().decode(
            UserSettings.self,
            from: Data(#"{"themePreset":"nous"}"#.utf8)
        )
        #expect(legacy.themePreset == .kallisti)

        // The new OLED preset persists.
        let oled = try JSONDecoder().decode(
            UserSettings.self,
            from: Data(#"{"themePreset":"kallistiOLED"}"#.utf8)
        )
        #expect(oled.themePreset == .kallistiOLED)
    }

    @Test("Kallisti OLED survives an encode/decode round-trip")
    func oledRoundTrip() throws {
        var settings = UserSettings()
        settings.themePreset = .kallistiOLED
        settings.colorSchemePreference = .dark
        let data = try JSONEncoder().encode(settings)
        let decoded = try JSONDecoder().decode(UserSettings.self, from: data)
        #expect(decoded.themePreset == .kallistiOLED)
        #expect(decoded.colorSchemePreference == .dark)
    }

    // MARK: - Texture bands

    @Test("Texture opacity stays inside the specified bands")
    func textureBands() {
        // Default theme: 4%.
        #expect(KallistiTheme.Texture.dark >= 0.03 && KallistiTheme.Texture.dark <= 0.08)
        // OLED: 0–3%.
        #expect(KallistiTheme.Texture.oled >= 0 && KallistiTheme.Texture.oled <= 0.03)
        // OLED must carry less grain than the default theme.
        #expect(KallistiTheme.Texture.oled < KallistiTheme.Texture.dark)
        // Marketing is allowed to be much heavier (15–45%).
        #expect(KallistiTheme.Texture.marketing >= 0.15 && KallistiTheme.Texture.marketing <= 0.45)
    }

    @Test("OLED watermark is more restrained than the default theme")
    func watermarkWeights() {
        #expect(ThemePreset.kallistiOLED.darkColors.watermarkOpacity
                < ThemePreset.kallisti.darkColors.watermarkOpacity)
    }

    // MARK: - Shape language

    @Test("Card radii sit in the 12–18pt band and compact controls in 22–28pt")
    func shapeLanguage() {
        #expect(Design.CornerRadius.md >= 12 && Design.CornerRadius.md <= 18)
        #expect(Design.CornerRadius.lg >= 12 && Design.CornerRadius.lg <= 18)
        #expect(Design.CornerRadius.xxl >= 22 && Design.CornerRadius.xxl <= 28)
    }

    @Test("Hairline borders never exceed full opacity")
    func hairlineBorders() {
        #expect(alpha(Design.Colors.border) <= 1.0)
        #expect(alpha(Design.Colors.borderStrong) <= 1.0)
        #expect(alpha(Design.Colors.divider) < alpha(Design.Colors.borderStrong))
    }

    // MARK: - Typography roles

    @Test("Typography separates display, body, and mono roles")
    func typographyRoles() {
        #expect(Design.Typography.body != Design.Typography.code)
        #expect(Design.Typography.heroTitle != Design.Typography.body)
        #expect(Design.Typography.eyebrow != Design.Typography.body)
        #expect(Design.Typography.body != Design.Typography.caption)
    }

    // MARK: - Motion bands

    @Test("Voice breathing period sits in the 1.8–2.4s band")
    func breathingBand() {
        #expect(Design.Motion.breatheDuration >= 1.8 && Design.Motion.breatheDuration <= 2.4)
    }

    @Test("Reduce Motion flattens breathing scale instead of animating size")
    func reduceMotionFlattensScale() {
        #expect(Design.Motion.breatheScale(reduceMotion: true) == 1.0)
        #expect(Design.Motion.breatheScale(reduceMotion: false) > 1.0)
    }

    // MARK: - Accessibility

    @Test("Reduce Transparency suppresses texture and solidifies surfaces")
    func reduceTransparency() {
        #expect(Design.A11y.textureOpacity(0.04, reduceTransparency: true) == 0)
        #expect(Design.A11y.textureOpacity(0.04, reduceTransparency: false) == 0.04)
        #expect(Design.A11y.surfaceOpacity(reduceTransparency: true) == 1.0)
        #expect(Design.A11y.surfaceOpacity(reduceTransparency: false) < 1.0)
    }

    @Test("Body text meets WCAG AA contrast in every Kallisti appearance")
    func bodyTextContrast() {
        let cases: [(String, ThemePalette)] = [
            ("Kallisti dark", ThemePreset.kallisti.darkColors),
            ("Kallisti light", ThemePreset.kallisti.lightColors),
            ("Kallisti OLED", ThemePreset.kallistiOLED.darkColors)
        ]
        for (name, p) in cases {
            let primary = contrastRatio(p.foreground, p.background)
            #expect(primary >= 4.5, "\(name): primary text contrast \(primary) < 4.5")
            let secondary = contrastRatio(p.secondaryForeground, p.background)
            #expect(secondary >= 4.5, "\(name): secondary text contrast \(secondary) < 4.5")
        }
    }

    @Test("Cards separate from the background in every Kallisti appearance")
    func cardSeparation() {
        // OLED especially: cards must still read against true black.
        for p in [ThemePreset.kallisti.darkColors, ThemePreset.kallistiOLED.darkColors] {
            #expect(hex(p.surface) != hex(p.background),
                    "card surface must differ from the ground")
            #expect(hex(p.surfaceRaised) != hex(p.surface),
                    "raised surface must differ from the base surface")
        }
    }

    // MARK: - Default appearance

    @Test("Kallisti is the launch default preset")
    @MainActor
    func kallistiIsDefault() {
        #expect(ThemeManager().preset == .kallisti)
        #expect(UserSettings().themePreset == .kallisti)
    }
}
