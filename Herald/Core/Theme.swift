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
/// The first five fields are the original (pre-2.1) contract. Kallisti needs a
/// richer vocabulary — layered surfaces, a real accent pair, semantic signals,
/// texture weight — so those are appended with defaults derived from the core
/// five. That keeps every pre-2.1 preset constructing exactly as before while
/// letting the Kallisti presets supply full fidelity.
struct ThemePalette {
    let background: Color
    let foreground: Color
    let secondaryForeground: Color
    let surface: Color
    let divider: Color

    // MARK: Kallisti additions

    /// Deepest ground, behind `background`. Used for gutters and scrims.
    let deepInk: Color
    /// Large dark fields and panels.
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
    /// `kallisti` is the Kallisti branded default (deep obsidian).
    /// `kallistiOLED` is the premium true-black variant. The remaining presets are
    /// pre-2.1 and kept intact as secondary options.
    case kallisti, kallistiOLED, midnight, ember, mono, cyberpunk, slate
    var id: String { rawValue }

    /// The two first-class Kallisti appearances, in Settings display order.
    static let kallistiPresets: [ThemePreset] = [.kallisti, .kallistiOLED]

    /// Pre-2.1 presets, retained as secondary options.
    static let legacyPresets: [ThemePreset] = [.midnight, .ember, .mono, .cyberpunk, .slate]

    var label: String {
        switch self {
        case .kallisti: "Kallisti"
        case .kallistiOLED: "Kallisti OLED"
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
        case .kallisti: "Deep obsidian. The Kallisti default."
        case .kallistiOLED: "True black. Sharper edges, less texture."
        case .midnight: "Violet on near-black."
        case .ember: "Warm red on brown-black."
        case .mono: "Neutral greyscale."
        case .cyberpunk: "High-contrast green terminal."
        case .slate: "Muted blue-grey."
        }
    }

    /// True for the Kallisti brand themes, which carry the full token set.
    var isKallistiBrand: Bool {
        self == .kallisti || self == .kallistiOLED
    }

    var accent: Color {
        switch self {
        case .kallisti: KallistiTheme.Obsidian.accent
        case .kallistiOLED: KallistiTheme.OLED.accent
        case .midnight: Color(hex: 0x8B5CF6)
        case .ember: Color(hex: 0xEF4444)
        case .mono: Color(hex: 0xA1A1AA)
        case .cyberpunk: Color(hex: 0x00FF41)
        case .slate: Color(hex: 0x64748B)
        }
    }

    var darkColors: ThemePalette {
        switch self {
        case .kallisti:
            return ThemePalette(
                background: KallistiTheme.Obsidian.background,
                foreground: KallistiTheme.Obsidian.bone,
                secondaryForeground: KallistiTheme.Obsidian.mist,
                surface: KallistiTheme.Obsidian.surface,
                divider: KallistiTheme.Obsidian.divider,
                deepInk: KallistiTheme.Obsidian.deepInk,
                panel: KallistiTheme.Obsidian.panel,
                surfaceRaised: KallistiTheme.Obsidian.surfaceRaised,
                surfaceSelected: KallistiTheme.Obsidian.selected,
                tertiaryForeground: KallistiTheme.Obsidian.pewter,
                accent: KallistiTheme.Obsidian.accent,
                accentHot: KallistiTheme.Obsidian.accentHot,
                success: KallistiTheme.Signal.success,
                warning: KallistiTheme.Signal.warning,
                danger: KallistiTheme.Signal.danger,
                textureOpacity: KallistiTheme.Texture.dark,
                watermarkOpacity: KallistiTheme.Mark.watermarkOpacity,
                prefersSharpEdges: false
            )
        case .kallistiOLED:
            return ThemePalette(
                background: KallistiTheme.OLED.background,
                foreground: KallistiTheme.OLED.foreground,
                secondaryForeground: KallistiTheme.OLED.secondary,
                surface: KallistiTheme.OLED.surface,
                divider: KallistiTheme.OLED.divider,
                deepInk: Color(hex: 0x000000),
                panel: KallistiTheme.OLED.surface,
                surfaceRaised: KallistiTheme.OLED.surfaceRaised,
                surfaceSelected: KallistiTheme.OLED.surfaceRaised,
                tertiaryForeground: KallistiTheme.OLED.tertiary,
                accent: KallistiTheme.OLED.accent,
                accentHot: KallistiTheme.OLED.accentHot,
                success: KallistiTheme.Signal.success,
                warning: KallistiTheme.Signal.warning,
                danger: KallistiTheme.Signal.danger,
                textureOpacity: KallistiTheme.Texture.oled,
                watermarkOpacity: KallistiTheme.Mark.watermarkOLED,
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
        case .kallisti, .kallistiOLED:
            return ThemePalette(
                background: KallistiTheme.Light.background,
                foreground: KallistiTheme.Light.foreground,
                secondaryForeground: KallistiTheme.Light.secondary,
                surface: KallistiTheme.Light.surface,
                divider: KallistiTheme.Light.divider,
                deepInk: KallistiTheme.Light.surface,
                panel: KallistiTheme.Light.surface,
                surfaceRaised: KallistiTheme.Light.surfaceRaised,
                surfaceSelected: KallistiTheme.Light.accentHot.opacity(0.16),
                tertiaryForeground: KallistiTheme.Light.tertiary,
                accent: KallistiTheme.Light.accent,
                accentHot: KallistiTheme.Light.accentHot,
                success: KallistiTheme.Signal.success,
                warning: KallistiTheme.Signal.warning,
                danger: KallistiTheme.Signal.danger,
                textureOpacity: KallistiTheme.Texture.light,
                watermarkOpacity: KallistiTheme.Mark.watermarkLight,
                prefersSharpEdges: self == .kallistiOLED
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
            return synthesizeLight(from: darkColors)
        }
    }

    private func synthesizeLight(from dark: ThemePalette) -> ThemePalette {
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

// MARK: - Kallisti Appearance (Settings-facing)

/// The four first-class appearances offered in Settings → Appearance.
///
/// Kallisti has two independent axes: a `ThemePreset` (color identity)
/// and a `ColorSchemePreference` (light/dark/system). Rather than add a redundant
/// `kallistiLight` preset, this type presents the sanctioned *combinations* as
/// single choices, and writes through to both stored axes — so existing
/// UserDefaults keys and their migrations are untouched.
enum KallistiAppearance: String, CaseIterable, Identifiable {
    /// Kallisti identity, following the iOS system light/dark setting.
    case system
    /// Kallisti branded dark — deep obsidian. The default.
    case kallisti
    /// Kallisti OLED — premium true black.
    case kallistiOLED
    /// Kallisti Light — clean paper, dark ink.
    case kallistiLight

    var id: String { rawValue }

    var label: String {
        switch self {
        case .system: "System"
        case .kallisti: "Kallisti"
        case .kallistiOLED: "Kallisti OLED"
        case .kallistiLight: "Kallisti Light"
        }
    }

    var detail: String {
        switch self {
        case .system: "Follows your iOS appearance setting."
        case .kallisti: "Deep obsidian. The Kallisti default."
        case .kallistiOLED: "True black. Sharper edges, less texture."
        case .kallistiLight: "Clean paper with dark ink."
        }
    }

    var symbol: String {
        switch self {
        case .system: "circle.lefthalf.filled"
        case .kallisti: "moon.stars.fill"
        case .kallistiOLED: "circle.fill"
        case .kallistiLight: "sun.max.fill"
        }
    }

    /// The stored axes this appearance corresponds to.
    var preset: ThemePreset {
        self == .kallistiOLED ? .kallistiOLED : .kallisti
    }

    var colorScheme: ColorSchemePreference {
        switch self {
        case .system: .system
        case .kallisti, .kallistiOLED: .dark
        case .kallistiLight: .light
        }
    }

    /// Swatch color representing the appearance in the picker.
    var swatch: Color {
        switch self {
        case .system: KallistiTheme.Obsidian.selected
        case .kallisti: KallistiTheme.Obsidian.surface
        case .kallistiOLED: Color(hex: 0x000000)
        case .kallistiLight: KallistiTheme.Light.background
        }
    }

    /// Resolve the current stored state to an appearance, or `nil` when a
    /// pre-2.1 preset is active (in which case Settings shows the legacy
    /// theme + light/dark controls instead).
    static func resolve(
        preset: ThemePreset,
        colorScheme: ColorSchemePreference
    ) -> KallistiAppearance? {
        switch preset {
        case .kallistiOLED:
            return .kallistiOLED
        case .kallisti:
            switch colorScheme {
            case .system: return .system
            case .dark: return .kallisti
            case .light: return .kallistiLight
            }
        default:
            return nil
        }
    }
}

// MARK: - Chat Wallpaper Rendering

/// Renders the background content for a given `ChatWallpaper` selection.
struct ChatWallpaperBackground: View {
    let wallpaper: ChatWallpaper

    var tint: Color = .accentColor

    @Environment(\.colorScheme) private var colorScheme
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

    /// Kallisti default wallpaper: the seated-Eris relief art on dark
    /// themes (brand asset), and the seated figure cropped from the light
    /// banner in light mode. Both render aspect-fit and centered so the
    /// figure stays fully visible on iPad landscape - scaledToFill on a
    /// portrait image in a landscape frame cropped the subject out and
    /// left only the dark field, which read as a faded/blank wallpaper.
    @ViewBuilder
    private var defaultBackground: some View {
        GeometryReader { proxy in
            if colorScheme == .dark {
                Image("KallistiWallpaper")
                    .resizable()
                    .scaledToFit()
                    .frame(width: proxy.size.width, height: proxy.size.height)
                    .clipped()
            } else {
                Image("KallistiWallpaperLight")
                    .resizable()
                    .scaledToFit()
                    .frame(width: proxy.size.width, height: proxy.size.height)
                    .clipped()
            }
        }
        .ignoresSafeArea()
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

/// Procedurally-drawn texture backgrounds.
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
/// dot patterns stable across redraws.
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
