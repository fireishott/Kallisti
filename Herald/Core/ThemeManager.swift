import SwiftUI

@MainActor
@Observable
final class ThemeManager {
    static let shared = ThemeManager()

    var preset: ThemePreset = .kallisti {
        didSet { syncSnapshot() }
    }
    var colorSchemePreference: ColorSchemePreference = .system {
        didSet { syncSnapshot() }
    }
    var systemScheme: ColorScheme = .dark {
        didSet { syncSnapshot() }
    }

    init() {
        syncSnapshot()
    }

    var currentScheme: ColorScheme {
        resolvedColorScheme(for: systemScheme)
    }

    func resolvedColorScheme(for systemScheme: ColorScheme) -> ColorScheme {
        switch colorSchemePreference {
        case .system: return systemScheme
        case .light: return .light
        case .dark: return .dark
        }
    }

    func currentPalette(for systemScheme: ColorScheme) -> ThemePalette {
        let resolved = resolvedColorScheme(for: systemScheme)
        return preset.colors(for: resolved)
    }

    var currentPalette: ThemePalette {
        preset.colors(for: currentScheme)
    }

    func load(from settings: UserSettings) {
        preset = settings.themePreset
        colorSchemePreference = settings.colorSchemePreference
        syncSnapshot()
    }

    func save(to settings: inout UserSettings) {
        settings.themePreset = preset
        settings.colorSchemePreference = colorSchemePreference
    }

    /// Publish the resolved palette to `ThemeSnapshot` so non-isolated color
    /// lookups (`Design.Colors`, `Design.Brand`, `SyntaxHighlighter`) can read it
    /// without hopping to the main actor — see `ThemeSnapshot` for why that
    /// matters.
    private func syncSnapshot() {
        ThemeSnapshot.update(currentPalette)
    }
}
