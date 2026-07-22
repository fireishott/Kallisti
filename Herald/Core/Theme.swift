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

struct ThemePalette {
    let background: Color
    let foreground: Color
    let secondaryForeground: Color
    let surface: Color
    let divider: Color
}

enum ThemePreset: String, Codable, CaseIterable, Identifiable {
    case herald, midnight, ember, mono, cyberpunk, slate
    var id: String { rawValue }

    var label: String {
        switch self {
        case .herald: "Herald"
        case .midnight: "Midnight"
        case .ember: "Ember"
        case .mono: "Mono"
        case .cyberpunk: "Cyberpunk"
        case .slate: "Slate"
        }
    }

    var accent: Color {
        switch self {
        case .herald: Color(hex: 0xFF6B00)
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
            return ThemePalette(
                background: Color(hex: 0x1A1D23),
                foreground: Color(hex: 0xF0F2F5),
                secondaryForeground: Color(hex: 0xF0F2F5).opacity(0.6),
                surface: Color.white.opacity(0.08),
                divider: Color.white.opacity(0.1)
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
        case .herald:
            return ThemePalette(
                background: Color(hex: 0xF8FAFC),
                foreground: Color(hex: 0x1E293B),
                secondaryForeground: Color(hex: 0x1E293B).opacity(0.6),
                surface: Color.black.opacity(0.04),
                divider: Color.black.opacity(0.1)
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

    @ViewBuilder
    private var defaultBackground: some View {
        ZStack {
            Color(.systemBackground)
            GeometryReader { geo in
                let markSize = min(geo.size.width * 0.9, 440)
                Image("AppIconImage")
                    .resizable()
                    .aspectRatio(contentMode: .fit)
                    .frame(width: markSize, height: markSize)
                    .opacity(0.18)
                    .position(x: geo.size.width * 0.5, y: geo.size.height * 0.35)
            }
            RadialGradient(
                colors: [tint.opacity(0.04), .clear],
                center: .center,
                startRadius: 0,
                endRadius: 500
            )
        }
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
