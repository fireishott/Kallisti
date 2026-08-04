import SwiftUI
import UIKit

enum ColorSchemePreference: String, Codable, CaseIterable, Identifiable {
    case system, light, dark
    var id: String { rawValue }
    var label: String {
        switch self {
        case .system: "System"
        case .light: "Light"
        case .dark: "Dark"
        }
    }
}

/// Resolved color set for one theme in one color scheme.
///
/// The first five fields are the original (pre-2.1) contract. Herald 2.1 needs a
/// richer vocabulary — layered surfaces, a real accent pair, semantic signals,
/// texture weight — so those are appended with defaults derived from the core
/// five. That keeps every pre-2.1 preset constructing exactly as before while
/// letting the Herald presets supply full fidelity.
struct ThemePalette {
    let background: Color
    let foreground: Color
    let secondaryForeground: Color
    let surface: Color
    let divider: Color

    // MARK: Herald 2.1 additions

    /// Deepest ground, behind `background`. Used for gutters and scrims.
    let deepInk: Color
    /// Large dark fields and panels (Herald: templeBlue).
    let panel: Color
    /// Elevated card surface, one step above `surface`.
    let surfaceRaised: Color
    /// Selected / active surface fill.
    let surfaceSelected: Color
    /// Tertiary text.
    let tertiaryForeground: Color
    /// Primary interactive color.
    let accent: Color
    /// Focus / progress / active-status color — brighter than `accent`.
    let accentHot: Color

    let success: Color
    let warning: Color
    let danger: Color

    /// Relief-print stipple opacity for backgrounds. 0 disables texture.
    let textureOpacity: Double
    /// Opacity for the icon watermark in wallpapers and empty states.
    let watermarkOpacity: Double
    /// OLED-style presentation: sharper card edges, lower halo.
    let prefersSharpEdges: Bool

    init(
        background: Color,
        foreground: Color,
        secondaryForeground: Color,
        surface: Color,
        divider: Color,
        deepInk: Color? = nil,
        panel: Color? = nil,
        surfaceRaised: Color? = nil,
        surfaceSelected: Color? = nil,
        tertiaryForeground: Color? = nil,
        accent: Color? = nil,
        accentHot: Color? = nil,
        success: Color = Color(hex: 0x00C275),
        warning: Color = Color(hex: 0xCF9A2F),
        danger: Color = Color(hex: 0xCF1322),
        textureOpacity: Double = 0,
        watermarkOpacity: Double = 0.18,
        prefersSharpEdges: Bool = false
    ) {
        self.background = background
        self.foreground = foreground
        self.secondaryForeground = secondaryForeground
        self.surface = surface
        self.divider = divider
        self.deepInk = deepInk ?? background
        self.panel = panel ?? surface
        self.surfaceRaised = surfaceRaised ?? surface
        self.surfaceSelected = surfaceSelected ?? divider
        self.tertiaryForeground = tertiaryForeground ?? secondaryForeground.opacity(0.85)
        self.accent = accent ?? divider
        self.accentHot = accentHot ?? (accent ?? divider)
        self.success = success
        self.warning = warning
        self.danger = danger
        self.textureOpacity = textureOpacity
        self.watermarkOpacity = watermarkOpacity
        self.prefersSharpEdges = prefersSharpEdges
    }
}

enum ThemePreset: String, Codable, CaseIterable, Identifiable {
    /// `herald` is the Herald 2.1 branded default (deep cobalt "Temple Blue").
    /// `heraldOLED` is the premium true-black variant. The remaining presets are
    /// pre-2.1 and kept intact as secondary options.
    case herald, heraldOLED, midnight, ember, mono, cyberpunk, slate
    var id: String { rawValue }

    /// The two first-class Herald 2.1 appearances, in Settings display order.
    static let heraldPresets: [ThemePreset] = [.herald, .heraldOLED]

    /// Pre-2.1 presets, retained as secondary options.
    static let legacyPresets: [ThemePreset] = [.midnight, .ember, .mono, .cyberpunk, .slate]

    var label: String {
        switch self {
        case .herald: "Kallisti"
        case .heraldOLED: "Kallisti OLED"
        case .midnight: "Midnight"
        case .ember: "Ember"
        case .mono: "Mono"
        case .cyberpunk: "Cyberpunk"
        case .slate: "Slate"
        }
    }

    /// Short description shown beneath the option in the Appearance section.
    var appearanceDescription: String {
        switch self {
        case .herald: "Deep cobalt. The Herald default."
        case .heraldOLED: "True black. Sharper edges, less texture."
        case .midnight: "Violet on near-black."
        case .ember: "Warm red on brown-black."
        case .mono: "Neutral greyscale."
        case .cyberpunk: "High-contrast green terminal."
        case .slate: "Muted blue-grey."
        }
    }

    /// True for the Herald 2.1 brand themes, which carry the full token set.
    var isHeraldBrand: Bool {
        self == .herald || self == .heraldOLED
    }

    var accent: Color {
        switch self {
        // Herald 2.1: signal blue. Orange is retired — it was never part of
        // the Herald identity, only the pre-2.1 placeholder accent.
        case .herald: HeraldTheme.Cobalt.signalBlue
        case .heraldOLED: HeraldTheme.OLED.accent
        case .midnight: Color(hex: 0x8B5CF6)
        case .ember: Color(hex: 0xEF4444)
        case .mono: Color(hex: 0xA1A1AA)
        case .cyberpunk: Color(hex: 0x00FF41)
        case .slate: Color(hex: 0x64748B)
        }
    }

    var darkColors: ThemePalette {
        switch self {
        case .herald:
            // Herald 2.1 default — deep cobalt. Controlled contrast on purpose:
            // the ground is a material, not a void.
            return ThemePalette(
                background: HeraldTheme.Cobalt.background,
                foreground: HeraldTheme.Cobalt.bone,
                secondaryForeground: HeraldTheme.Cobalt.mist,
                surface: HeraldTheme.Cobalt.surface,
                divider: HeraldTheme.Cobalt.divider,
                deepInk: HeraldTheme.Cobalt.deepInk,
                panel: HeraldTheme.Cobalt.templeBlue,
                surfaceRaised: HeraldTheme.Cobalt.surfaceRaised,
                surfaceSelected: HeraldTheme.Cobalt.royalBlue,
                tertiaryForeground: HeraldTheme.Cobalt.steel,
                accent: HeraldTheme.Cobalt.signalBlue,
                accentHot: HeraldTheme.Cobalt.signalBlueHot,
                success: HeraldTheme.Signal.success,
                warning: HeraldTheme.Signal.warning,
                danger: HeraldTheme.Signal.danger,
                textureOpacity: HeraldTheme.Texture.cobalt,
                watermarkOpacity: HeraldTheme.Mark.watermarkOpacity,
                prefersSharpEdges: false
            )
        case .heraldOLED:
            // Premium black. Near-black ground rather than #000 so cards can
            // still separate; true black is reserved for the deepest layer.
            return ThemePalette(
                background: HeraldTheme.OLED.nearBlack,
                foreground: HeraldTheme.OLED.foreground,
                secondaryForeground: HeraldTheme.OLED.secondary,
                surface: HeraldTheme.OLED.surface,
                divider: HeraldTheme.OLED.divider,
                deepInk: HeraldTheme.OLED.black,
                panel: HeraldTheme.OLED.surface,
                surfaceRaised: HeraldTheme.OLED.surfaceRaised,
                surfaceSelected: HeraldTheme.OLED.surfaceRaised,
                tertiaryForeground: HeraldTheme.OLED.tertiary,
                accent: HeraldTheme.OLED.accent,
                accentHot: HeraldTheme.OLED.accentHot,
                success: HeraldTheme.Signal.success,
                warning: HeraldTheme.Signal.warning,
                danger: HeraldTheme.Signal.danger,
                textureOpacity: HeraldTheme.Texture.oled,
                watermarkOpacity: HeraldTheme.Mark.watermarkOpacityOLED,
                prefersSharpEdges: true
            )
        case .midnight:
            return ThemePalette(
                background: Color(hex: 0x0F0A1A),
                foreground: Color(hex: 0xE8E0F0),
                secondaryForeground: Color(hex: 0xE8E0F0).opacity(0.6),
                surface: Color.white.opacity(0.06),
                divider: Color.white.opacity(0.08)
            )
        case .ember:
            return ThemePalette(
                background: Color(hex: 0x1A1210),
                foreground: Color(hex: 0xF5E6D3),
                secondaryForeground: Color(hex: 0xF5E6D3).opacity(0.6),
                surface: Color.white.opacity(0.06),
                divider: Color.white.opacity(0.08)
            )
        case .mono:
            return ThemePalette(
                background: Color(hex: 0x18181B),
                foreground: Color(hex: 0xFAFAFA),
                secondaryForeground: Color(hex: 0xFAFAFA).opacity(0.6),
                surface: Color.white.opacity(0.06),
                divider: Color.white.opacity(0.08)
            )
        case .cyberpunk:
            return ThemePalette(
                background: Color(hex: 0x0A0A0A),
                foreground: Color(hex: 0x00FF41),
                secondaryForeground: Color(hex: 0x00FF41).opacity(0.6),
                surface: Color(hex: 0x00FF41).opacity(0.05),
                divider: Color(hex: 0x00FF41).opacity(0.15)
            )
        case .slate:
            return ThemePalette(
                background: Color(hex: 0x0F172A),
                foreground: Color(hex: 0xE2E8F0),
                secondaryForeground: Color(hex: 0xE2E8F0).opacity(0.6),
                surface: Color.white.opacity(0.06),
                divider: Color.white.opacity(0.08)
            )
        }
    }

    var lightColors: ThemePalette {
        switch self {
        // Both Herald brand themes share one light counterpart: cool paper,
        // cobalt ink. "OLED light" is a contradiction, so heraldOLED resolves
        // here too rather than inventing a washed-out black theme.
        case .herald, .heraldOLED:
            return ThemePalette(
                background: HeraldTheme.Light.background,
                foreground: HeraldTheme.Light.foreground,
                secondaryForeground: HeraldTheme.Light.secondary,
                surface: HeraldTheme.Light.surface,
                divider: HeraldTheme.Light.divider,
                deepInk: HeraldTheme.Light.surface,
                panel: HeraldTheme.Light.surface,
                surfaceRaised: HeraldTheme.Light.surfaceRaised,
                surfaceSelected: HeraldTheme.Light.accentHot.opacity(0.16),
                tertiaryForeground: HeraldTheme.Light.tertiary,
                accent: HeraldTheme.Light.accent,
                accentHot: HeraldTheme.Light.accentHot,
                success: HeraldTheme.Signal.success,
                warning: HeraldTheme.Signal.warning,
                danger: HeraldTheme.Signal.danger,
                textureOpacity: HeraldTheme.Texture.light,
                watermarkOpacity: 0.10,
                prefersSharpEdges: self == .heraldOLED
            )
        case .mono:
            return ThemePalette(
                background: Color(hex: 0xFAFAFA),
                foreground: Color(hex: 0x18181B),
                secondaryForeground: Color(hex: 0x18181B).opacity(0.6),
                surface: Color.black.opacity(0.04),
                divider: Color.black.opacity(0.1)
            )
        default:
            // Auto-synthesize from dark colors
            return synthesizeLight(from: darkColors)
        }
    }

    private func synthesizeLight(from dark: ThemePalette) -> ThemePalette {
        // Invert luminance: dark BG -> light BG, dark FG -> light FG
        return ThemePalette(
            background: Color(hex: 0xF5F5F5),
            foreground: Color(hex: 0x1A1A1A),
            secondaryForeground: Color(hex: 0x1A1A1A).opacity(0.6),
            surface: Color.black.opacity(0.04),
            divider: Color.black.opacity(0.1)
        )
    }

    func colors(for scheme: ColorScheme) -> ThemePalette {
        scheme == .dark ? darkColors : lightColors
    }
}

// MARK: - Herald Appearance (Settings-facing)

/// The four first-class appearances offered in Settings → Appearance.
///
/// Herald has always had two independent axes: a `ThemePreset` (color identity)
/// and a `ColorSchemePreference` (light/dark/system). Rather than add a redundant
/// `heraldLight` preset, this type presents the sanctioned *combinations* as
/// single choices, and writes through to both stored axes — so existing
/// UserDefaults keys and their migrations are untouched.
enum HeraldAppearance: String, CaseIterable, Identifiable {
    /// Herald identity, following the iOS system light/dark setting.
    case system
    /// Herald branded dark — deep cobalt. The 2.1 default.
    case herald
    /// Herald OLED — premium true black.
    case heraldOLED
    /// Herald Light — cool paper, cobalt ink.
    case heraldLight

    var id: String { rawValue }

    var label: String {
        switch self {
        case .system: "System"
        case .herald: "Kallisti"
        case .heraldOLED: "Kallisti OLED"
        case .heraldLight: "Kallisti Light"
        }
    }

    var detail: String {
        switch self {
        case .system: "Follows your iOS appearance setting."
        case .herald: "Deep cobalt. The Herald default."
        case .heraldOLED: "True black. Sharper edges, less texture."
        case .heraldLight: "Cool paper with cobalt ink."
        }
    }

    var symbol: String {
        switch self {
        case .system: "circle.lefthalf.filled"
        case .herald: "moon.stars.fill"
        case .heraldOLED: "circle.fill"
        case .heraldLight: "sun.max.fill"
        }
    }

    /// The stored axes this appearance corresponds to.
    var preset: ThemePreset {
        self == .heraldOLED ? .heraldOLED : .herald
    }

    var colorScheme: ColorSchemePreference {
        switch self {
        case .system: .system
        case .herald, .heraldOLED: .dark
        case .heraldLight: .light
        }
    }

    /// Swatch color representing the appearance in the picker.
    var swatch: Color {
        switch self {
        case .system: HeraldTheme.Cobalt.royalBlue
        case .herald: HeraldTheme.Cobalt.surface
        case .heraldOLED: HeraldTheme.OLED.black
        case .heraldLight: HeraldTheme.Light.background
        }
    }

    /// Resolve the current stored state to an appearance, or `nil` when a
    /// pre-2.1 preset is active (in which case Settings shows the legacy
    /// theme + light/dark controls instead).
    static func resolve(
        preset: ThemePreset,
        colorScheme: ColorSchemePreference
    ) -> HeraldAppearance? {
        switch preset {
        case .heraldOLED:
            return .heraldOLED
        case .herald:
            switch colorScheme {
            case .system: return .system
            case .dark: return .herald
            case .light: return .heraldLight
            }
        default:
            return nil
        }
    }
}

// MARK: - Chat Wallpaper Rendering

/// Renders the background content for a given `ChatWallpaper` selection.
///
/// This is the single rendering primitive the chat wallpaper feature (screen
/// background + picker thumbnails) should consume: every style is drawn
/// procedurally — gradients via `LinearGradient`/`RadialGradient`, textures via
/// `Canvas` — instead of shipped as baked PNG assets, so the same view scales
/// cleanly from a full-screen background down to a small swatch.
struct ChatWallpaperBackground: View {
    let wallpaper: ChatWallpaper

    /// Tint used for `.solid` and for the `.default` placeholder mark. Defaults
    /// to the system accent color; callers may pass the active `ThemePreset`'s
    /// `accent` to keep the wallpaper in sync with the selected theme.
    var tint: Color = .accentColor

    /// Cache decoded custom image to avoid re-decoding on every render.
    @State private var cachedCustomImage: UIImage?

    var body: some View {
        switch wallpaper {
        case .default:
            defaultBackground
        case .gradient1:
            LinearGradient(
                colors: [Color(hex: 0xFF7E5F), Color(hex: 0xFEB47B)],
                startPoint: .topLeading,
                endPoint: .bottomTrailing
            )
        case .gradient2:
            LinearGradient(
                colors: [Color(hex: 0x2E3192), Color(hex: 0x1BFFFF)],
                startPoint: .top,
                endPoint: .bottom
            )
        case .gradient3:
            LinearGradient(
                colors: [Color(hex: 0x134E5E), Color(hex: 0x71B280)],
                startPoint: .topLeading,
                endPoint: .bottomTrailing
            )
        case .gradient4:
            RadialGradient(
                colors: [Color(hex: 0x8E2DE2), Color(hex: 0x4A00E0), Color(hex: 0x00C9FF)],
                center: .topLeading,
                startRadius: 0,
                endRadius: 900
            )
        case .texture1:
            ChatWallpaperTexture(style: .paper)
        case .texture2:
            ChatWallpaperTexture(style: .noise)
        case .solid:
            tint
        case .custom(let data):
            customImage(data)
        }
    }

    /// Herald 2.1 default wallpaper: deep ink ground, restrained blue field, and
    /// a low-opacity icon watermark. Resolves per theme — cobalt under Herald,
    /// near-black with a pulled-back glow under Herald OLED — so the single
    /// `.default` selection is correct in every appearance.
    @ViewBuilder
    private var defaultBackground: some View {
        HeraldBrandField(isReadingSurface: true)
    }

    @ViewBuilder
    private func customImage(_ data: Data) -> some View {
        if let image = cachedCustomImage {
            Image(uiImage: image)
                .resizable()
                .scaledToFill()
        } else {
            Color(.secondarySystemBackground)
                .task {
                    cachedCustomImage = UIImage(data: data)
                }
        }
    }
}

/// Procedurally-drawn texture backgrounds (`.texture1` "Paper", `.texture2`
/// "Noise"). Dots are placed with a seeded PRNG so the pattern is stable across
/// re-renders instead of resampling — and visually flickering — every time
/// SwiftUI re-invokes the `Canvas` closure.
private struct ChatWallpaperTexture: View {
    enum Style {
        case paper
        case noise
    }

    let style: Style

    var body: some View {
        Canvas { context, size in
            let base: Color
            let dot: Color
            let count: Int
            let seed: UInt64
            let radiusRange: ClosedRange<Double>

            switch style {
            case .paper:
                base = Color(hex: 0xF5F0E6)
                dot = Color(hex: 0xD8CFB8)
                count = 260
                seed = 42
                radiusRange = 0.5...1.2
            case .noise:
                base = Color(hex: 0x1C1C1E)
                dot = Color.white.opacity(0.25)
                count = 900
                seed = 7
                radiusRange = 0.4...1.6
            }

            context.fill(Path(CGRect(origin: .zero, size: size)), with: .color(base))

            var generator = ChatWallpaperSeededGenerator(seed: seed)
            for _ in 0..<count {
                let x = Double.random(in: 0...max(size.width, 1), using: &generator)
                let y = Double.random(in: 0...max(size.height, 1), using: &generator)
                let radius = Double.random(in: radiusRange, using: &generator)
                let rect = CGRect(x: x - radius, y: y - radius, width: radius * 2, height: radius * 2)
                context.fill(Path(ellipseIn: rect), with: .color(dot))
            }
        }
    }
}

/// Small deterministic PRNG (xorshift64) used only to keep texture wallpaper
/// dot patterns stable across redraws. Not for cryptographic or gameplay use.
private struct ChatWallpaperSeededGenerator: RandomNumberGenerator {
    private var state: UInt64

    init(seed: UInt64) {
        state = seed &+ 0x9E3779B97F4A7C15
    }

    mutating func next() -> UInt64 {
        state ^= state << 13
        state ^= state >> 7
        state ^= state << 17
        return state
    }
}
