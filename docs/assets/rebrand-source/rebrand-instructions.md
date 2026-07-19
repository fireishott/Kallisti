# HERALD - Complete Rebrand Instructions

## Overview

This document contains the full specification for rebranding the Hermes-iOS
fork to **HERALD**. Every change listed here must be completed. This is a
complete identity overhaul - naming, visuals, licensing, documentation.

---

## 1. Identity

| Field | Old Value | New Value |
|-------|-----------|-----------|
| **Product Name** | Hermes iOS / HermesMobile | HERALD |
| **Bundle ID** | com.herald.legacy | net.fihonline.herald |
| **Xcode Project** | HermesMobile.xcodeproj | Herald.xcodeproj |
| **Xcode Scheme** | HermesMobile | Herald |
| **App Target** | HermesMobile | Herald |
| **Module Name** | HermesMobile | Herald |
| **GitHub Repo** | fireishott/Hermes-iOS | fireishott/Herald (or similar) |
| **Relay Container** | hermes-relay | herald-relay |
| **Relay DNS** | hermes-relay.fihonline.net | herald-relay.fihonline.net (or subdomain of choice) |
| **Connector Package** | hermes_mobile_connector | herald_connector |

---

## 2. Brand Assets

### App Icon
- Place the final app icon PNG (1024x1024) at `Hermel/Resources/AppIcon.png`
- The icon is a single angular flame on pure black background
- Molten orange (#FF6B00) to white-hot (#FFF5E0) gradient
- No text on the icon

### Brand Mark (Landscape)
- Single flame left, "HERALD" text right
- Flame: angular, aggressive, sharp-edged vertical flame
- Text: wide-tracked bold sans-serif, warm off-white (#F5F0E8)
- No tagline

### Color Palette
- Primary: #FF6B00 (molten orange)
- Hot tip: #FFF5E0 (white-hot)
- Background: #0A0A0A (pure black)
- Surface: #1A1D23 (dark charcoal)
- Text: #F5F0E8 (warm off-white)
- Accent shadow: #FF3D00 (ember red)

---

## 3. Code Changes

### 3.1 Rename Xcode Project

The project file must be renamed and all internal references updated:
- `HermesMobile.xcodeproj/` -> `Herald.xcodeproj/`
- Inside `project.pbxproj`: replace all occurrences of `HermesMobile` with `Herald`
- Update `PRODUCT_BUNDLE_IDENTIFIER` from `com.herald.legacy` to `net.fihonline.herald`
- Update `PRODUCT_NAME` from `HermesMobile` to `Herald`
- Update `DEVELOPMENT_TEAM` to `58U7UPFS53` (unchanged, but verify)

### 3.2 Rename Swift Module

All Swift files referencing the `HermesMobile` module:
- `import HermesMobile` -> `import Herald`
- Module name in pbxproj `PRODUCT_MODULE_NAME` -> `Herald`

### 3.3 Class/Struct Renames

These are the key types that contain "Hermes" in their name. Rename them:

| Old Name | New Name |
|----------|----------|
| `HermesAvatar` | `HeraldAvatar` |
| `HermesHostStore` | `HeraldHostStore` |
| `HermesHostStatus` | `HeraldHostStatus` |
| `HermesActivityAttributes` | `HeraldActivityAttributes` |
| `HermesWidgetData` | `HeraldWidgetData` |
| `HermesClientProtocol` | `HeraldClientProtocol` |
| `HermesHostServiceProtocol` | `HeraldHostServiceProtocol` |
| `LiveHermesClient` | `LiveHeraldClient` |
| `LiveHermesHostService` | `LiveHeraldHostService` |
| `MockHermesClient` | `MockHeraldClient` |
| `MockHermesHostService` | `MockHeraldHostService` |
| `HermesMobile.CarPlaySceneDelegate` | `Herald.CarPlaySceneDelegate` |
| `HermesMobile.AppEntry` | `Herald.AppEntry` |

**Note:** Files named with "hermes" in the filename should also be renamed:
- `HermesHostStore.swift` -> `HeraldHostStore.swift`
- `HermesHostStatus.swift` -> `HeraldHostStatus.swift`
- etc.

### 3.4 String Literals

Search all Swift files for string literals containing "Hermes" or "hermes":
- Display names shown to users: update to "Herald"
- Log messages: update to "Herald"
- Error messages: update to "Herald"
- Notification titles: update to "Herald"

Do NOT rename internal variable names that are lowercase/camelCase unless they
are user-facing. Focus on what users see.

### 3.5 Theme Preset

In `Core/Theme.swift`, rename the `.nous` preset:
- `.nous` -> `.herald` (or `.ember` or another fire-themed name)
- Update the label from `"Nous"` to `"Herald"` (or chosen name)
- Keep the accent color as the brand orange (#FF6B00 or similar)
- Update `ThemeManager.swift` default preset from `.nous` to new name

### 3.6 Info.plist

Update in `Herald/Resources/Info.plist`:
- `CFBundleDisplayName` -> `Herald`
- `CFBundleName` -> `Herald`
- Keep version strings as-is (they'll be bumped separately)

### 3.7 Entitlements

Update bundle identifier in:
- `Herald/Herald.entitlements` (renamed from HermesMobile.entitlements)
- Any App Group identifiers if present

---

## 4. Relay Changes

### 4.1 Container Name
- `hermes-relay` -> `hererald-relay` (update docker-compose.yml)

### 4.2 Environment Variables
In relay `.env` and config:
- `APNS_BUNDLE_ID=net.fihonline.herald`
- Any references to `hermes-relay` in URLs -> `herald-relay`

### 4.3 Relay Source
In `relay/app/`:
- Update any "Hermes" references in user-facing strings
- `hermes_adapter.py` -> rename class/function references to Herald
- Keep internal variable names functional, update display names

---

## 5. Connector Changes

### 5.1 Package Name
- `hermes_mobile_connector` -> `herald_connector`
- Update `pyproject.toml` name field
- Update import statements in all Python files

### 5.2 CLI
- Update CLI help text and display names
- Keep command structure the same

---

## 6. Documentation

### 6.1 README.md - Full Overhaul

The README is the front door. It must be professional, visually polished,
and immediately communicate what HERALD is. Complete rewrite from scratch.

#### Structure (top to bottom):

1. **Hero Banner** - Full-width SVG brand mark (flame + "HERALD" text) centered
   - Use the landscape brand mark SVG, max-height 120px
   - Below: one-line tagline in muted text: "Self-hosted AI companion for iPhone and iPad"

2. **Badges Row** - Shield.io badges on a single line:
   - `![](https://img.shields.io/badge/version-1.0.0-FF6B00)` (orange badge)
   - `![](https://img.shields.io/badge/license-MIT-F5F0E8)` (off-white badge)
   - `![](https://img.shields.io/badge/platform-iOS%2026+-0A0A0A)` (dark badge with orange text)
   - `![](https://img.shields.io/badge/self--hosted-yes-FF3D00)` (ember badge)
   - `![](https://img.shields.io/badge/relay-active-FF6B00)` (orange badge)

3. **App Icon + Description Block** - Two columns:
   - Left: App icon PNG, 96x96, rounded corners
   - Right: 2-3 sentence description of what HERALD is and why it exists
   - "HERALD is a native iOS companion for self-hosted AI runtimes. It adds
     voice mode, sensors, CarPlay, session management, and a relay so your
     AI moves between your phone, tablet, and desktop without becoming a
     hosted service."

4. **Screenshot Gallery** - Three inline images side by side (use HTML <img>
   tags for consistent sizing):
   - iPhone chat screen
   - iPad sidebar layout
   - Voice mode / talk screen
   - Images stored in `docs/screenshots/` directory
   - Each image: max-width 30%, rounded corners, subtle border

5. **Features Grid** - Use an HTML table with two columns for clean layout:

   | Feature | Description |
   |---------|-------------|
   | **Streaming Chat** | Real-time streaming with markdown, code blocks, inline diffs, and attachments |
   | **Voice Mode** | OpenAI Realtime voice with live camera context and Hermes tool delegation |
   | **iPad Native** | Full NavigationSplitView layout with session browser sidebar |
   | **Session Management** | Pin, archive, rename, search. Device-scoped sessions. |
   | **Model Switching** | Switch models on the fly via direct RPC |
   | **Sensors** | Health, location, motion data piped to your AI in real-time |
   | **CarPlay** | Hands-free AI from your dashboard |
   | **Themes** | 6 built-in presets with custom wallpaper support |
   | **Cron Jobs** | Schedule recurring AI tasks from your phone |
   | **Skills Browser** | Browse and manage installed agent skills |

6. **Architecture Diagram** - An SVG diagram showing:
   ```
   [iPhone/iPad] <--HTTPS--> [Relay] <--MCP--> [Connector] <---> [Hermes Runtime]
   ```
   - Clean, minimal, dark theme matching the brand
   - Color the connecting lines in molten orange (#FF6B00)
   - Nodes in dark surface (#1A1D23) with off-white text (#F5F0E8)
   - Store as `docs/architecture.svg`

7. **Quick Start** - Numbered steps with code blocks:
   ```markdown
   ## Quick Start

   1. **Deploy the relay**
      ```bash
      docker compose up -d
      ```

   2. **Install the connector**
      ```bash
      pip install herald-connector
      herald-connector start
      ```

   3. **Install HERALD on your iPhone**
      - Build from source (see below) or download the latest release
      - Open the app, scan the pairing QR code
      - Start chatting with your AI
   ```

8. **Building from Source** - Concise build instructions:
   - Prerequisites (Xcode 26+, macOS 26+, Apple Developer account)
   - Clone, open project, build
   - Link to full build docs in `docs/BUILDING.md`

9. **Relay Configuration** - Table of env vars with defaults:
   ```
   | Variable | Default | Description |
   |----------|---------|-------------|
   | RELAY_ENVIRONMENT | development | production or development |
   | PUBLIC_BASE_URL | http://localhost:8000/v1 | Public relay URL |
   | APNS_KEY_ID | - | APNs key ID for push notifications |
   | APNS_TEAM_ID | - | Apple Developer team ID |
   | APNS_BUNDLE_ID | net.fihonline.herald | App bundle ID |
   ```

10. **Contributing** - Link to CONTRIBUTING.md with a brief note

11. **Acknowledgements** - MANDATORY section:
    ```
    ## Acknowledgements

    Built on the foundation of [Hermes-iOS](https://github.com/dylan-buck/Hermes-iOS)
    by [Dylan Buck](https://github.com/dylan-buck) and the
    [Nous Research](https://nousresearch.com/) community.
    Original work licensed under MIT.
    ```

12. **License** - `MIT` with link to LICENSE file

#### README Style Rules:

- Use `<!-- HERALD -->` comment at the very top as a marker
- All images use relative paths (`docs/screenshots/`, `docs/architecture.svg`)
- No external image URLs except shields.io badges
- Dark theme implied by repo description, but README renders fine on light too
- Keep total README under 300 lines. Dense but not exhausting.
- Use `<br>` sparingly for spacing, not massive markdown gaps
- Code blocks always specify language (```bash, ```swift, ```python)

#### SVG Assets to Create:

Store all brand SVGs in `docs/assets/`:

| File | Description | Specs |
|------|-------------|-------|
| `brand-mark.svg` | Landscape: flame + "HERALD" text | Width 600px, height 120px, transparent bg |
| `brand-mark-vertical.svg` | Stacked: flame on top, "HERALD" below | Width 200px, height 240px, transparent bg |
| `app-icon.svg` | Flame only (for favicon/small use) | 512x512, transparent bg |
| `architecture.svg` | System architecture diagram | Width 800px, dark bg #0A0A0A |
| `icon-192.png` | PWA/icon size | 192x192 PNG from SVG |
| `icon-512.png` | PWA/icon size | 512x512 PNG from SVG |

The SVGs should use the brand palette:
- Flame: `#FF6B00` fill with `#FFF5E0` highlight
- Text: `#F5F0E8` fill
- Background: transparent (for use on any background)

#### Screenshot Directory:

Create `docs/screenshots/` and populate with actual device screenshots:
| File | Content |
|------|---------|
| `iphone-chat.png` | iPhone chat screen with a conversation |
| `ipad-sidebar.png` | iPad split view with sidebar |
| `voice-mode.png` | Voice/talk mode active |
| `settings.png` | Settings screen with themes |
| `pairing.png` | QR code pairing flow |

All screenshots: PNG, @2x resolution preferred, dark mode.

### 6.2 CHANGELOG.md
- Keep existing entries (they're historical fact)
- Add a new entry at top: version bump + rebrand note
- Add a new entry at top: version bump + rebrand note
- Format: `## [1.0.0] - YYYY-MM-DD` with "Rebrand from Hermes-iOS to HERALD"

### 6.3 CONTRIBUTING.md
- Update project name references
- Update any GitHub URLs

### 6.4 SECURITY.md
- Update project name references

### 6.5 MAINTAINER_NOTES.md
- Update all references to Hermes/HermesMobile

---

## 7. Licensing & Attribution

### 7.1 License File
The current LICENSE is MIT with "Copyright (c) 2026 Hermes iOS Contributors".
Update to:
```
MIT License

Copyright (c) 2026 Herald Contributors
Copyright (c) 2026 Original Hermes iOS Contributors
```

### 7.2 Attribution
This project is a fork of Hermes-iOS by Nous Research / Dylan Buck.
The following attribution MUST be preserved:

**In README.md, add a section:**
```
## Acknowledgements

HERALD is built on the foundation of
[Hermes-iOS](https://github.com/dylan-buck/Hermes-iOS) by
[Dylan Buck](https://github.com/dylan-buck) and the
[Nous Research](https://nousresearch.com/) community.
Original work is licensed under MIT.
```

**In the LICENSE file**, retain the original copyright holder alongside the new one.

### 7.3 Upstream Remote
Keep the upstream remote pointing to the original repo:
```
git remote add upstream https://github.com/dylan-buck/Hermes-iOS.git
```
This preserves the fork relationship on GitHub.

---

## 8. GitHub Repository - Professional Overhaul

### 8.1 Repo Rename
- Rename from `Hermes-iOS` to `Herald` (or `herald-ios` if preferred)
- GitHub auto-redirects old URLs
- Update local remote: `git remote set-url origin https://github.com/fireishott/Herald.git`

### 8.2 Repository Settings

**Description:**
"Self-hosted AI companion for iPhone and iPad. Native iOS client with relay, voice mode, sensors, and CarPlay."

**Website:** (if applicable, e.g. herald.fihonline.net or similar)

**Topics:**
`herald` `ai-companion` `self-hosted` `ios` `ipad` `carplay` `voice-mode` `sensors` `swift` `python` `mcp`

**Social Preview:**
Upload the brand-mark.svg (or a PNG export of it) as the social preview image.
GitHub uses 1280x640 for social cards. Create a dedicated file:
- `docs/social-preview.png` - 1280x640, brand mark centered on black background

### 8.3 Directory Structure (after rebrand)

The repo should have a clean, professional structure:

```
Herald/
├── Herald/                        # iOS app source
│   ├── AppEntry.swift
│   ├── ContentView.swift
│   ├── CarPlay/
│   ├── Components/
│   ├── Core/
│   │   ├── Design.swift
│   │   ├── Theme.swift
│   │   ├── ThemeManager.swift
│   │   ├── Router.swift
│   │   └── ...
│   ├── Features/
│   │   ├── Chat/
│   │   ├── Cron/
│   │   ├── Inbox/
│   │   ├── Onboarding/
│   │   ├── Permissions/
│   │   ├── Settings/
│   │   ├── Sidebar/
│   │   ├── Skills/
│   │   └── Talk/
│   ├── Models/
│   ├── Resources/
│   │   ├── Assets.xcassets/
│   │   └── Info.plist
│   ├── Services/
│   │   ├── Live/
│   │   ├── Mocks/
│   │   ├── Protocols/
│   │   └── Support/
│   └── Stores/
├── Herald.xcodeproj/              # Renamed from HermesMobile
├── relay/                         # Python relay server
│   ├── app/
│   │   ├── main.py
│   │   ├── services.py
│   │   └── ...
│   ├── Dockerfile
│   ├── docker-compose.yml
│   └── pyproject.toml
├── connector/                     # Python MCP connector
│   ├── src/
│   │   └── herald_connector/
│   ├── tests/
│   └── pyproject.toml
├── docs/
│   ├── assets/
│   │   ├── brand-mark.svg
│   │   ├── brand-mark-vertical.svg
│   │   ├── app-icon.svg
│   │   └── architecture.svg
│   ├── screenshots/
│   │   ├── iphone-chat.png
│   │   ├── ipad-sidebar.png
│   │   ├── voice-mode.png
│   │   └── settings.png
│   ├── social-preview.png
│   ├── BUILDING.md
│   └── CONFIGURATION.md
├── skills/
│   └── herald-ios/
│       └── SKILL.md
├── .gitignore
├── CHANGELOG.md
├── CONTRIBUTING.md
├── LICENSE
├── README.md
├── SECURITY.md
├── project.yml
└── MAINTAINER_NOTES.md
```

### 8.4 New Files to Create

| File | Purpose |
|------|---------|
| `docs/BUILDING.md` | Full build instructions (move from README) |
| `docs/CONFIGURATION.md` | Relay and connector config reference |
| `docs/assets/*.svg` | Brand assets (flame icon, landscape mark, vertical mark) |
| `docs/architecture.svg` | System architecture diagram |
| `docs/screenshots/*.png` | Device screenshots |
| `docs/social-preview.png` | GitHub social card image |
| `CONTRIBUTING.md` | Contribution guidelines |
| `SECURITY.md` | Security policy |
| `.github/FUNDING.yml` | Optional: sponsorship links |

### 8.5 GitHub Features to Enable

- **Releases**: Create v1.0.0 release with the signed IPA attached as a release asset
- **Discussions**: Enable for community Q&A
- **Wiki**: Optional, for extended documentation
- **Pages**: Enable from `docs/` directory for a project website (optional)

### 8.6 Branch Protection (Optional but Recommended)

- Protect `main` branch
## 9. Deployment References

### 9.1 Build Script Updates
Any build scripts or CI that reference:
- `HermesMobile.xcodeproj` -> `Herald.xcodeproj`
- `HermesMobile` scheme -> `Herald` scheme
- `HermesMobile.ipa` -> `Herald.ipa`
- Bundle ID in signing commands -> `net.fihonline.herald`

### 9.2 Relay Deployment
- Docker container name: `herald-relay`
- DNS: update if changing subdomain
- `.env` file: update bundle ID and any Hermes references

### 9.3 iOS App
- After rename, the app will appear as "Herald" on the home screen
- Users will need to re-pair after a fresh install (signing team change = new data container)

---

## 10. What NOT to Change

- **Git history** - preserve all commits, this is a rename not a squash
- **Upstream remote** - keep pointing to dylan-buck/Hermes-iOS
- **License type** - stay MIT
- **Architecture** - the code structure is good, don't reorganize for the sake of it
- **Test targets** - update names but keep test coverage intact
- **Relay API endpoints** - `/v1/` prefix stays the same, don't break the API
- **Connector MCP tools** - tool names can stay functional, update display names only

---

## 11. Verification Checklist

After completing all changes:

- [ ] Project builds cleanly (`xcodebuild build` with no errors)
- [ ] App installs on device with new bundle ID
- [ ] App displays "Herald" on home screen
- [ ] Relay starts with new container name
- [ ] Connector connects to relay successfully
- [ ] Pairing flow works end-to-end
- [ ] All theme presets render correctly
- [ ] No "Hermes" text visible in the UI (check settings, about, errors)
- [ ] README loads correctly on GitHub with new branding
- [ ] LICENSE shows correct attribution
- [ ] GitHub repo name is updated
- [ ] Fork relationship to upstream is preserved

---

## 12. Version

Start HERALD at version **1.0.0**. This is a new product, not a minor update.
Bump both `MARKETING_VERSION` and `CFBundleShortVersionString` to `1.0.0`.
Reset `CURRENT_PROJECT_VERSION` and `CFBundleVersion` to `1`.
