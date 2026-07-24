# Themes, Wallpaper, and Session History Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add 6 theme presets (matching Electron), light/dark/system mode, chat background wallpaper, and full-text session search to the Hermes iOS app.

**Architecture:** Theme state flows via `@Observable ThemeManager` in the SwiftUI environment. `Design.Colors` becomes adaptive, reading from the active theme. Wallpaper is rendered behind the message list. Session search is a relay-side change joining the messages table.

**Tech Stack:** Swift/SwiftUI (iOS), Python/FastAPI (relay)

## Global Constraints

- 6 theme presets match Electron names: Nous, Midnight, Ember, Mono, Cyberpunk, Slate
- Light variants auto-synthesized for dark-only themes (invert luminance)
- Design.Colors tokens become computed properties reading from ThemeManager
- Wallpaper applies to chat background only (not full app)
- Default wallpaper = Hermes logo silhouette matching Electron
- Session search joins messages table for full-text search
- Cross-device toggle persisted in UserSettings

---

## Task 1: Theme Presets and ThemeManager

**Files:**
- Create: `HermesMobile/Core/Theme.swift`
- Create: `HermesMobile/Core/ThemeManager.swift`

**Interfaces:**
- Produces: `ThemePreset` enum, `ThemePalette` struct, `ThemeManager` @Observable class
- Consumed by: All views via Design.Colors, Settings UI

- [ ] **Step 1: Create Theme.swift**

```swift
import SwiftUI

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
    case nous, midnight, ember, mono, cyberpunk, slate
    var id: String { rawValue }

    var label: String {
        switch self {
        case .nous: "Nous"
        case .midnight: "Midnight"
        case .ember: "Ember"
        case .mono: "Mono"
        case .cyberpunk: "Cyberpunk"
        case .slate: "Slate"
        }
    }

    var accent: Color {
        switch self {
        case .nous: Color(hex: 0x4A9EFF)
        case .midnight: Color(hex: 0x8B5CF6)
        case .ember: Color(hex: 0xEF4444)
        case .mono: Color(hex: 0xA1A1AA)
        case .cyberpunk: Color(hex: 0x00FF41)
        case .slate: Color(hex: 0x64748B)
        }
    }

    var darkColors: ThemePalette {
        switch self {
        case .nous:
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
        case .nous:
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
```

- [ ] **Step 2: Create ThemeManager.swift**

```swift
import SwiftUI

@MainActor
@Observable
final class ThemeManager {
    static let shared = ThemeManager()

    var preset: ThemePreset = .nous
    var colorSchemePreference: ColorSchemePreference = .system

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

    func load(from settings: UserSettings) {
        preset = settings.themePreset
        colorSchemePreference = settings.colorSchemePreference
    }

    func save(to settings: inout UserSettings) {
        settings.themePreset = preset
        settings.colorSchemePreference = colorSchemePreference
    }
}
```

- [ ] **Step 3: Add ThemePreset and ColorSchemePreference to UserSettings**

In `HermesMobile/Models/UserSettings.swift`, add:
```swift
var themePreset: ThemePreset = .nous
var colorSchemePreference: ColorSchemePreference = .system
```

- [ ] **Step 4: Wire ThemeManager into AppContainer and AppEntry**

Add `themeManager` property to AppContainer. Inject via `.environment()` in AppEntry. Load from UserSettings on init, save on change.

- [ ] **Step 5: Commit**

```bash
cd ~/Hermes-iOS
git add HermesMobile/Core/Theme.swift HermesMobile/Core/ThemeManager.swift HermesMobile/Models/UserSettings.swift HermesMobile/Stores/AppContainer.swift HermesMobile/AppEntry.swift
git commit -m "feat(ios): add theme presets and ThemeManager"
```

---

## Task 2: Make Design.Colors Adaptive

**Files:**
- Modify: `HermesMobile/Core/Design.swift`

**Interfaces:**
- Consumes: `ThemeManager.shared`
- Produces: Adaptive color tokens that respond to theme changes

- [ ] **Step 1: Replace hardcoded Design.Colors with computed properties**

Change from:
```swift
enum Colors {
    static let background = Color(hex: 0x2D2D2B)
    static let foreground = Color(hex: 0xF9F9F7)
    // ...
}
```

To:
```swift
enum Colors {
    @MainActor
    static var background: Color { ThemeManager.shared.currentPalette(for: .dark).background }
    @MainActor
    static var foreground: Color { ThemeManager.shared.currentPalette(for: .dark).foreground }
    // ...
}
```

**Note:** Since `Design.Colors` is used in many places as static properties, and SwiftUI's `@Environment(\.colorScheme)` isn't available in static context, the approach is:
- `ThemeManager` holds the current `ColorScheme` (updated by the root view)
- `Design.Colors` reads from `ThemeManager.shared.currentPalette`
- The root view in `ContentView` updates `ThemeManager.currentScheme` via `.onTraitCollectionChange` or `@Environment(\.colorScheme)`

- [ ] **Step 2: Add currentScheme to ThemeManager**

```swift
var currentScheme: ColorScheme = .dark  // Updated by root view
```

- [ ] **Step 3: Update ContentView to sync color scheme**

```swift
@Environment(\.colorScheme) private var systemColorScheme
.onChange(of: systemColorScheme) { _, newValue in
    themeManager.currentScheme = themeManager.resolvedColorScheme(for: newValue)
}
```

- [ ] **Step 4: Update Design.Brand.accent to be adaptive**

```swift
enum Brand {
    @MainActor
    static var accent: Color { ThemeManager.shared.preset.accent }
}
```

- [ ] **Step 5: Commit**

```bash
cd ~/Hermes-iOS
git add HermesMobile/Core/Design.swift HermesMobile/ContentView.swift
git commit -m "feat(ios): make Design.Colors adaptive to theme"
```

---

## Task 3: Appearance Settings UI

**Files:**
- Modify: `HermesMobile/Features/Settings/SettingsScreen.swift`

**Interfaces:**
- Consumes: `ThemeManager`, `UserSettings`
- Produces: Appearance section in Settings

- [ ] **Step 1: Add Appearance section to SettingsScreen**

```swift
// MARK: - Appearance Section
SettingsSectionView(title: "Appearance") {
    // Theme preset picker
    VStack(alignment: .leading, spacing: 8) {
        Text("Theme")
            .font(Design.Typography.footnote)
            .foregroundStyle(Design.Colors.secondaryForeground)
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 12) {
                ForEach(ThemePreset.allCases) { theme in
                    themeSwatch(theme)
                }
            }
        }
    }

    Divider()

    // Light/Dark/System toggle
    VStack(alignment: .leading, spacing: 8) {
        Text("Appearance")
            .font(Design.Typography.footnote)
            .foregroundStyle(Design.Colors.secondaryForeground)
        Picker("Appearance", selection: $themeManager.colorSchemePreference) {
            ForEach(ColorSchemePreference.allCases) { pref in
                Text(pref.label).tag(pref)
            }
        }
        .pickerStyle(.segmented)
    }

    Divider()

    // Wallpaper picker entry
    NavigationLink {
        WallpaperPickerSheet()
    } label: {
        HStack {
            Text("Chat Wallpaper")
            Spacer()
            Text(currentWallpaperLabel)
                .foregroundStyle(.secondary)
            Image(systemName: "chevron.right")
                .font(.caption)
                .foregroundStyle(.tertiary)
        }
    }
}
```

- [ ] **Step 2: Add themeSwatch helper view**

```swift
private func themeSwatch(_ theme: ThemePreset) -> some View {
    Button {
        themeManager.preset = theme
        // Save to UserSettings
    } label: {
        VStack(spacing: 4) {
            Circle()
                .fill(theme.accent)
                .frame(width: 32, height: 32)
                .overlay(
                    Circle()
                        .strokeBorder(
                            themeManager.preset == theme ? Color.white : Color.clear,
                            lineWidth: 2
                        )
                )
            Text(theme.label)
                .font(.caption2)
                .foregroundStyle(Design.Colors.secondaryForeground)
        }
    }
    .buttonStyle(.plain)
}
```

- [ ] **Step 3: Commit**

```bash
cd ~/Hermes-iOS
git add HermesMobile/Features/Settings/SettingsScreen.swift
git commit -m "feat(ios): add Appearance settings section with theme picker"
```

---

## Task 4: Wallpaper Assets and Storage

**Files:**
- Create: `HermesMobile/Resources/Wallpapers/` — bundled wallpaper images
- Modify: `HermesMobile/Models/UserSettings.swift` — add ChatWallpaper enum

**Interfaces:**
- Produces: `ChatWallpaper` enum, bundled wallpaper assets
- Consumed by: ChatScreen, WallpaperPickerSheet

- [ ] **Step 1: Create ChatWallpaper enum**

```swift
enum ChatWallpaper: Codable, Equatable, Identifiable {
    case `default`
    case gradient1, gradient2, gradient3, gradient4
    case texture1, texture2
    case solid
    case custom(Data)

    var id: String {
        switch self {
        case .default: "default"
        case .gradient1: "gradient1"
        case .gradient2: "gradient2"
        case .gradient3: "gradient3"
        case .gradient4: "gradient4"
        case .texture1: "texture1"
        case .texture2: "texture2"
        case .solid: "solid"
        case .custom: "custom"
        }
    }

    var label: String {
        switch self {
        case .default: "Default"
        case .gradient1: "Sunset"
        case .gradient2: "Ocean"
        case .gradient3: "Forest"
        case .gradient4: "Aurora"
        case .texture1: "Paper"
        case .texture2: "Noise"
        case .solid: "Solid"
        case .custom: "Photo"
        }
    }

    var thumbnailName: String? {
        switch self {
        case .gradient1: "wallpaper-gradient-1"
        case .gradient2: "wallpaper-gradient-2"
        case .gradient3: "wallpaper-gradient-3"
        case .gradient4: "wallpaper-gradient-4"
        case .texture1: "wallpaper-texture-1"
        case .texture2: "wallpaper-texture-2"
        default: nil
        }
    }
}
```

- [ ] **Step 2: Add to UserSettings**

```swift
var chatWallpaper: ChatWallpaper = .default
```

- [ ] **Step 3: Create wallpaper placeholder assets**

Create gradient PNG images (1080x1920) for the 4 gradient presets and 2 texture presets. Add to `HermesMobile/Resources/Wallpapers/` and to the Xcode asset catalog.

- [ ] **Step 4: Create Hermes logo silhouette asset**

Create `hermes-logo-silhouette.png` — centered logo at ~15% opacity on transparent background. This is the default wallpaper matching Electron.

- [ ] **Step 5: Commit**

```bash
cd ~/Hermes-iOS
git add HermesMobile/Models/UserSettings.swift HermesMobile/Resources/Wallpapers/
git commit -m "feat(ios): add ChatWallpaper enum and bundled wallpaper assets"
```

---

## Task 5: Wallpaper Rendering in ChatScreen

**Files:**
- Modify: `HermesMobile/Features/Chat/ChatScreen.swift`

**Interfaces:**
- Consumes: `UserSettings.chatWallpaper`
- Produces: Wallpaper background behind message list

- [ ] **Step 1: Add wallpaper background to ChatScreen**

Wrap the message list in a ZStack with the wallpaper behind it:

```swift
ZStack {
    // Wallpaper background
    wallpaperBackground
        .ignoresSafeArea()

    // Existing message list and UI
    VStack(spacing: 0) {
        // ... existing chat content
    }
}
```

- [ ] **Step 2: Implement wallpaperBackground computed property**

```swift
@ViewBuilder
private var wallpaperBackground: some View {
    let wallpaper = settingsStore.userSettings.chatWallpaper
    switch wallpaper {
    case .default:
        // Hermes logo silhouette - centered, low opacity
        Design.Colors.background
            .overlay(
                Image("hermes-logo-silhouette")
                    .resizable()
                    .aspectRatio(contentMode: .fit)
                    .frame(width: 200)
                    .opacity(0.15)
            )
    case .solid:
        Design.Colors.background
    case .custom(let data):
        if let uiImage = UIImage(data: data) {
            Image(uiImage: uiImage)
                .resizable()
                .aspectRatio(contentMode: .fill)
                .blur(radius: 3)
                .opacity(0.4)
                .overlay(Color.black.opacity(0.3))
        } else {
            Design.Colors.background
        }
    default:
        if let name = wallpaper.thumbnailName {
            Image(name)
                .resizable()
                .aspectRatio(contentMode: .fill)
                .opacity(0.4)
                .overlay(Color.black.opacity(0.2))
        } else {
            Design.Colors.background
        }
    }
}
```

- [ ] **Step 3: Commit**

```bash
cd ~/Hermes-iOS
git add HermesMobile/Features/Chat/ChatScreen.swift
git commit -m "feat(ios): render chat wallpaper background in ChatScreen"
```

---

## Task 6: Wallpaper Picker UI

**Files:**
- Create: `HermesMobile/Features/Settings/WallpaperPickerSheet.swift`

**Interfaces:**
- Consumes: `UserSettings.chatWallpaper`, `PhotosPicker`
- Produces: Wallpaper selection UI in Settings

- [ ] **Step 1: Create WallpaperPickerSheet.swift**

```swift
import SwiftUI
import PhotosUI

struct WallpaperPickerSheet: View {
    @Environment(SettingsStore.self) private var settingsStore
    @State private var selectedItem: PhotosPickerItem?

    private let presets: [ChatWallpaper] = [
        .default, .gradient1, .gradient2, .gradient3, .gradient4,
        .texture1, .texture2, .solid
    ]

    var body: some View {
        ScrollView {
            VStack(spacing: 24) {
                // Preset grid
                LazyVGrid(columns: [
                    GridItem(.flexible()),
                    GridItem(.flexible()),
                    GridItem(.flexible())
                ], spacing: 16) {
                    ForEach(presets) { wallpaper in
                        wallpaperTile(wallpaper)
                    }
                }

                Divider()

                // Photo picker
                PhotosPicker(selection: $selectedItem, matching: .images) {
                    HStack {
                        Image(systemName: "photo.on.rectangle")
                        Text("Choose Photo")
                    }
                    .frame(maxWidth: .infinity)
                    .padding()
                    .background(Design.Colors.surface)
                    .clipShape(RoundedRectangle(cornerRadius: 12))
                }
            }
            .padding()
        }
        .navigationTitle("Chat Wallpaper")
        .navigationBarTitleDisplayMode(.inline)
        .onChange(of: selectedItem) { _, newItem in
            Task {
                if let data = try? await newItem?.loadTransferable(type: Data.self) {
                    settingsStore.updateWallpaper(.custom(data))
                }
            }
        }
    }

    private func wallpaperTile(_ wallpaper: ChatWallpaper) -> some View {
        Button {
            settingsStore.updateWallpaper(wallpaper)
        } label: {
            VStack(spacing: 8) {
                wallpaperThumbnail(wallpaper)
                    .frame(height: 120)
                    .clipShape(RoundedRectangle(cornerRadius: 12))
                    .overlay(
                        RoundedRectangle(cornerRadius: 12)
                            .strokeBorder(
                                settingsStore.userSettings.chatWallpaper == wallpaper
                                    ? Design.Brand.accent : Color.clear,
                                lineWidth: 3
                            )
                    )
                Text(wallpaper.label)
                    .font(.caption)
                    .foregroundStyle(Design.Colors.secondaryForeground)
            }
        }
        .buttonStyle(.plain)
    }

    @ViewBuilder
    private func wallpaperThumbnail(_ wallpaper: ChatWallpaper) -> some View {
        switch wallpaper {
        case .default:
            Design.Colors.background
                .overlay(
                    Image("hermes-logo-silhouette")
                        .resizable()
                        .aspectRatio(contentMode: .fit)
                        .frame(width: 60)
                        .opacity(0.3)
                )
        case .solid:
            Design.Colors.background
        case .custom:
            Design.Colors.surface
                .overlay(Image(systemName: "photo"))
        default:
            if let name = wallpaper.thumbnailName {
                Image(name)
                    .resizable()
                    .aspectRatio(contentMode: .fill)
            } else {
                Design.Colors.surface
            }
        }
    }
}
```

- [ ] **Step 2: Add updateWallpaper to SettingsStore**

```swift
func updateWallpaper(_ wallpaper: ChatWallpaper) {
    userSettings.chatWallpaper = wallpaper
    persist()
}
```

- [ ] **Step 3: Commit**

```bash
cd ~/Hermes-iOS
git add HermesMobile/Features/Settings/WallpaperPickerSheet.swift HermesMobile/Stores/SettingsStore.swift
git commit -m "feat(ios): add wallpaper picker with presets and photo library"
```

---

## Task 7: Full-Text Session Search (Relay)

**Files:**
- Modify: `relay/app/services.py` — `search_sessions()` function

**Interfaces:**
- Consumes: `conversations` + `messages` tables
- Produces: Search results matching title OR message content

- [ ] **Step 1: Modify search_sessions to join messages table**

In `relay/app/services.py`, update `search_sessions()`:

```python
def search_sessions(user_id: str, query: str, device_id: str | None = None, limit: int = 50) -> list:
    with get_session() as session:
        # Subquery: conversations whose messages match
        message_match = (
            select(Message.conversation_id)
            .where(Message.user_id == user_id)
            .where(Message.text.ilike(f"%{query}%"))
            .distinct()
            .subquery()
        )

        # Main query: title match OR message match
        stmt = (
            select(Conversation)
            .where(Conversation.user_id == user_id)
            .where(Conversation.is_archived == False)
            .where(
                or_(
                    Conversation.title.ilike(f"%{query}%"),
                    Conversation.id.in(select(message_match.c.conversation_id))
                )
            )
        )

        if device_id:
            stmt = stmt.where(
                or_(Conversation.device_id == device_id, Conversation.device_id.is_(None))
            )

        stmt = stmt.order_by(Conversation.last_message_at.desc().nullslast()).limit(limit)

        results = session.execute(stmt).scalars().all()
        return [serialize_session_summary(c) for c in results]
```

- [ ] **Step 2: Test the endpoint**

```bash
curl -s "https://your-relay.example.com/v1/sessions/search?q=test" -H "Authorization: Bearer <token>"
```

- [ ] **Step 3: Commit**

```bash
cd ~/Hermes-iOS
git add relay/app/services.py
git commit -m "feat(relay): add full-text message search to session search endpoint"
```

---

## Task 8: Cross-Device Session Filter (iOS)

**Files:**
- Modify: `HermesMobile/Stores/SessionListStore.swift`
- Modify: `HermesMobile/Features/Sidebar/iPadSidebarView.swift`
- Modify: `HermesMobile/Features/Sidebar/iPhoneSessionDrawer.swift`

**Interfaces:**
- Consumes: `UserSettings.showAllDevices`
- Produces: Toggle in session filter area

- [ ] **Step 1: Add showAllDevices to UserSettings**

```swift
var showAllDevices: Bool = false
```

- [ ] **Step 2: Update SessionListStore to respect the toggle**

In `loadSessions()`, when `showAllDevices` is true, don't pass `device_id` filter to the API call.

- [ ] **Step 3: Add toggle to filter UI**

In both iPadSidebarView and iPhoneSessionDrawer, add a "All Devices" toggle near the filter chips:

```swift
Toggle("All Devices", isOn: $sessionStore.showAllDevices)
    .font(.caption)
    .tint(Design.Brand.accent)
```

- [ ] **Step 4: Commit**

```bash
cd ~/Hermes-iOS
git add HermesMobile/Models/UserSettings.swift HermesMobile/Stores/SessionListStore.swift HermesMobile/Features/Sidebar/
git commit -m "feat(ios): add cross-device session filter toggle"
```

---

## Task 9: Deploy and Verify

- [ ] **Step 1: Push to GitHub**

```bash
cd ~/Hermes-iOS && git push origin master
```

- [ ] **Step 2: Deploy relay changes**

Update relay on ignyte host with new search_sessions function. Rebuild and restart Docker container.

- [ ] **Step 3: Build and install iOS app**

Build on MBP, install on iPhone and iPad. Verify:
- Theme picker works (all 6 presets)
- Light/Dark/System toggle works
- Wallpaper renders behind chat messages
- Wallpaper picker shows presets + photo library
- Session search finds messages by content
- All Devices toggle shows cross-device sessions

- [ ] **Step 4: Final commit**

```bash
cd ~/Hermes-iOS
git commit --allow-empty -m "chore: verify themes, wallpaper, session search end-to-end"
```
