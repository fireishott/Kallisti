import SwiftUI
import Testing
import UIKit
@testable import Kallisti

/// Herald 2.1 rebrand contract tests.
///
/// These lock the design tokens to the values in the rebrand package
/// (`palette/herald-palette.json` + BUILDER_THEME_PROMPT.md) so a future edit
/// can't silently drift the brand, and assert the accessibility behaviors the
/// spec requires (Dynamic Type, Reduce Motion, Reduce Transparency, contrast).
@Suite
struct HeraldThemeTests {

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

    @Test("Herald cobalt tokens match the published brand palette")
    func cobaltTokens() {
        #expect(hex(HeraldTheme.Cobalt.deepInk) == 0x020813)
        #expect(hex(HeraldTheme.Cobalt.background) == 0x030C1C)
        #expect(hex(HeraldTheme.Cobalt.templeBlue) == 0x071C3D)
        #expect(hex(HeraldTheme.Cobalt.surface) == 0x0C3569)
        #expect(hex(HeraldTheme.Cobalt.surfaceRaised) == 0x123F78)
        #expect(hex(HeraldTheme.Cobalt.royalBlue) == 0x1A4F97)
        #expect(hex(HeraldTheme.Cobalt.signalBlue) == 0x306FD6)
        #expect(hex(HeraldTheme.Cobalt.signalBlueHot) == 0x5797F1)
        #expect(hex(HeraldTheme.Cobalt.bone) == 0xF1F5F3)
        #expect(hex(HeraldTheme.Cobalt.mist) == 0xBCCDDA)
        #expect(hex(HeraldTheme.Cobalt.steel) == 0x789AB8)
        #expect(hex(HeraldTheme.Cobalt.divider) == 0x466C96)
    }

    @Test("Herald OLED tokens match the published brand palette")
    func oledTokens() {
        #expect(hex(HeraldTheme.OLED.black) == 0x000000)
        #expect(hex(HeraldTheme.OLED.nearBlack) == 0x05070B)
        #expect(hex(HeraldTheme.OLED.surface) == 0x0A1220)
        #expect(hex(HeraldTheme.OLED.surfaceRaised) == 0x0F1C30)
        #expect(hex(HeraldTheme.OLED.accent) == 0x3D7BFF)
        #expect(hex(HeraldTheme.OLED.accentHot) == 0x7AB0FF)
        #expect(hex(HeraldTheme.OLED.foreground) == 0xF5F7FA)
        #expect(hex(HeraldTheme.OLED.secondary) == 0xA8B7CC)
        #expect(hex(HeraldTheme.OLED.tertiary) == 0x6F84A0)
        #expect(hex(HeraldTheme.OLED.divider) == 0x24344D)
    }

    @Test("Herald Light tokens match the published brand palette")
    func lightTokens() {
        #expect(hex(HeraldTheme.Light.background) == 0xE9EEF2)
        #expect(hex(HeraldTheme.Light.surface) == 0xDCE5EC)
        #expect(hex(HeraldTheme.Light.foreground) == 0x061A38)
        #expect(hex(HeraldTheme.Light.secondary) == 0x355A7D)
        #expect(hex(HeraldTheme.Light.accent) == 0x1A4F97)
        #expect(hex(HeraldTheme.Light.divider) == 0x789AB8)
    }

    @Test("Semantic signal colors match the published brand palette")
    func signalTokens() {
        #expect(hex(HeraldTheme.Signal.success) == 0x41C98E)
        #expect(hex(HeraldTheme.Signal.warning) == 0xD9AF53)
        #expect(hex(HeraldTheme.Signal.danger) == 0xCF4D57)
    }

    // MARK: - Preset wiring

    @Test("Herald default dark palette is wired to the cobalt tokens")
    func heraldDarkPalette() {
        let p = ThemePreset.herald.darkColors
        #expect(hex(p.background) == 0x030C1C)
        #expect(hex(p.deepInk) == 0x020813)
        #expect(hex(p.panel) == 0x071C3D)
        #expect(hex(p.surface) == 0x0C3569)
        #expect(hex(p.surfaceRaised) == 0x123F78)
        #expect(hex(p.surfaceSelected) == 0x1A4F97)
        #expect(hex(p.foreground) == 0xF1F5F3)
        #expect(hex(p.secondaryForeground) == 0xBCCDDA)
        #expect(hex(p.tertiaryForeground) == 0x789AB8)
        #expect(hex(p.accent) == 0x306FD6)
        #expect(hex(p.accentHot) == 0x5797F1)
        #expect(p.prefersSharpEdges == false)
    }

    @Test("Herald OLED palette is wired to the OLED tokens, with true-black deep ink")
    func heraldOLEDPalette() {
        let p = ThemePreset.heraldOLED.darkColors
        #expect(hex(p.deepInk) == 0x000000, "OLED deepest layer must be true black")
        #expect(hex(p.background) == 0x05070B)
        #expect(hex(p.surface) == 0x0A1220)
        #expect(hex(p.surfaceRaised) == 0x0F1C30)
        #expect(hex(p.accent) == 0x3D7BFF)
        #expect(hex(p.accentHot) == 0x7AB0FF)
        #expect(p.prefersSharpEdges == true, "OLED presentation uses sharper edges")
    }

    @Test("Herald light palette is wired to the light tokens")
    func heraldLightPalette() {
        let p = ThemePreset.herald.lightColors
        #expect(hex(p.background) == 0xE9EEF2)
        #expect(hex(p.surface) == 0xDCE5EC)
        #expect(hex(p.foreground) == 0x061A38)
        #expect(hex(p.accent) == 0x1A4F97)
    }

    @Test("Herald OLED resolves to the shared light counterpart in light mode")
    func oledLightFallsBackToHeraldLight() {
        #expect(hex(ThemePreset.heraldOLED.lightColors.background) == 0xE9EEF2)
    }

    // MARK: - Orange is retired

    @Test("No Herald preset uses the retired orange accents")
    func orangeIsRetired() {
        let retired: Set<UInt> = [0xFF6B00, 0xFF3F00, 0xFF3D00, 0xFEB47B, 0xFF7E5F]
        for preset in ThemePreset.heraldPresets {
            #expect(!retired.contains(hex(preset.accent)),
                    "\(preset.rawValue) accent must not be orange")
            for palette in [preset.darkColors, preset.lightColors] {
                #expect(!retired.contains(hex(palette.accent)))
                #expect(!retired.contains(hex(palette.accentHot)))
                #expect(!retired.contains(hex(palette.foreground)))
                #expect(!retired.contains(hex(palette.background)))
            }
        }
    }

    @Test("Herald accents are the signal blues")
    func heraldAccentsAreBlue() {
        #expect(hex(ThemePreset.herald.accent) == 0x306FD6)
        #expect(hex(ThemePreset.heraldOLED.accent) == 0x3D7BFF)
    }

    // MARK: - Appearance mapping

    @Test("Every Herald appearance round-trips through its stored axes")
    func appearanceRoundTrip() {
        for appearance in HeraldAppearance.allCases {
            let resolved = HeraldAppearance.resolve(
                preset: appearance.preset,
                colorScheme: appearance.colorScheme
            )
            #expect(resolved == appearance,
                    "\(appearance.rawValue) did not round-trip")
        }
    }

    @Test("Appearance list is exactly System, Herald, Herald OLED, Herald Light")
    func appearanceRoster() {
        #expect(HeraldAppearance.allCases.map(\.label) ==
                ["System", "Herald", "Herald OLED", "Herald Light"])
    }

    @Test("A pre-2.1 preset resolves to no Herald appearance")
    func legacyPresetHasNoAppearance() {
        #expect(HeraldAppearance.resolve(preset: .slate, colorScheme: .dark) == nil)
        #expect(HeraldAppearance.resolve(preset: .cyberpunk, colorScheme: .light) == nil)
    }

    @Test("Pre-2.1 presets are retained as secondary options")
    func legacyPresetsRetained() {
        #expect(ThemePreset.legacyPresets == [.midnight, .ember, .mono, .cyberpunk, .slate])
        // All presets remain selectable/decodable — no case was removed.
        for raw in ["midnight", "ember", "mono", "cyberpunk", "slate", "herald"] {
            #expect(ThemePreset(rawValue: raw) != nil)
        }
    }

    // MARK: - Persistence

    @Test("themePreset default is Herald and legacy 'nous' still migrates")
    func presetPersistence() throws {
        // Absent key -> Herald default.
        let empty = try JSONDecoder().decode(UserSettings.self, from: Data("{}".utf8))
        #expect(empty.themePreset == .herald)

        // The 1.0.0 rename migration must survive the 2.1 rebrand.
        let legacy = try JSONDecoder().decode(
            UserSettings.self,
            from: Data(#"{"themePreset":"nous"}"#.utf8)
        )
        #expect(legacy.themePreset == .herald)

        // The new OLED preset persists.
        let oled = try JSONDecoder().decode(
            UserSettings.self,
            from: Data(#"{"themePreset":"heraldOLED"}"#.utf8)
        )
        #expect(oled.themePreset == .heraldOLED)
    }

    @Test("Herald OLED survives an encode/decode round-trip")
    func oledRoundTrip() throws {
        var settings = UserSettings()
        settings.themePreset = .heraldOLED
        settings.colorSchemePreference = .dark
        let data = try JSONEncoder().encode(settings)
        let decoded = try JSONDecoder().decode(UserSettings.self, from: data)
        #expect(decoded.themePreset == .heraldOLED)
        #expect(decoded.colorSchemePreference == .dark)
    }

    // MARK: - Texture bands

    @Test("Texture opacity stays inside the specified bands")
    func textureBands() {
        // Default theme: 3–8% in-app.
        #expect(HeraldTheme.Texture.cobalt >= 0.03 && HeraldTheme.Texture.cobalt <= 0.08)
        // OLED: 0–3%.
        #expect(HeraldTheme.Texture.oled >= 0 && HeraldTheme.Texture.oled <= 0.03)
        // OLED must carry less grain than the default theme.
        #expect(HeraldTheme.Texture.oled < HeraldTheme.Texture.cobalt)
        // Marketing is allowed to be much heavier (15–45%).
        #expect(HeraldTheme.Texture.marketing >= 0.15 && HeraldTheme.Texture.marketing <= 0.45)
    }

    @Test("OLED watermark is more restrained than the default theme")
    func watermarkWeights() {
        #expect(ThemePreset.heraldOLED.darkColors.watermarkOpacity
                < ThemePreset.herald.darkColors.watermarkOpacity)
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
        // Pre-2.1 these were `.opacity(1.5)` / `.opacity(2.75)`, which clamped
        // and flattened the whole border hierarchy.
        #expect(alpha(Design.Colors.border) <= 1.0)
        #expect(alpha(Design.Colors.borderStrong) <= 1.0)
        #expect(alpha(Design.Colors.divider) < alpha(Design.Colors.borderStrong))
    }

    // MARK: - Typography roles

    @Test("Typography separates display, body, and mono roles")
    func typographyRoles() {
        // Distinct roles must not collapse onto one another.
        #expect(Design.Typography.body != Design.Typography.code)
        #expect(Design.Typography.heroTitle != Design.Typography.body)
        #expect(Design.Typography.eyebrow != Design.Typography.body)
        // Body copy is no longer monospaced (pre-2.1 it was).
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
        #expect(Design.A11y.textureOpacity(0.055, reduceTransparency: true) == 0)
        #expect(Design.A11y.textureOpacity(0.055, reduceTransparency: false) == 0.055)
        #expect(Design.A11y.surfaceOpacity(reduceTransparency: true) == 1.0)
        #expect(Design.A11y.surfaceOpacity(reduceTransparency: false) < 1.0)
    }

    @Test("Body text meets WCAG AA contrast in every Herald appearance")
    func bodyTextContrast() {
        let cases: [(String, ThemePalette)] = [
            ("Herald dark", ThemePreset.herald.darkColors),
            ("Herald light", ThemePreset.herald.lightColors),
            ("Herald OLED", ThemePreset.heraldOLED.darkColors)
        ]
        for (name, p) in cases {
            let primary = contrastRatio(p.foreground, p.background)
            #expect(primary >= 4.5, "\(name): primary text contrast \(primary) < 4.5")
            let secondary = contrastRatio(p.secondaryForeground, p.background)
            #expect(secondary >= 4.5, "\(name): secondary text contrast \(secondary) < 4.5")
        }
    }

    @Test("Cards separate from the background in every Herald appearance")
    func cardSeparation() {
        // OLED especially: cards must still read against true black.
        for p in [ThemePreset.herald.darkColors, ThemePreset.heraldOLED.darkColors] {
            #expect(hex(p.surface) != hex(p.background),
                    "card surface must differ from the ground")
            #expect(hex(p.surfaceRaised) != hex(p.surface),
                    "raised surface must differ from the base surface")
        }
    }

    // MARK: - Default appearance

    @Test("Herald is the launch default preset")
    @MainActor
    func heraldIsDefault() {
        #expect(ThemeManager().preset == .herald)
        #expect(UserSettings().themePreset == .herald)
    }
}
