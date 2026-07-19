# Rich Chat Enhancements + HERALD Rebrand Design

**Date:** 2026-07-19
**Status:** Approved

---

## Overview

Two parallel deliverables that ship together as one release:

1. **HERALD Rebrand** — Complete identity overhaul of the Hermes-iOS fork. Naming, visuals, licensing, documentation. Spec source: `~/Desktop/herald-rebrand/rebrand-instructions.md`.
2. **Rich Chat Enhancements** — Structured message rendering, collaborative canvas, long-press interactions, and typed bubble treatments for tool calls, reasoning traces, and diffs.

**Ship sequence:** Rebrand first (touches every file), Rich Chat on the rebranded codebase (new files + targeted edits).

---

## Part 1: HERALD Rebrand

Full spec in `~/Desktop/herald-rebrand/rebrand-instructions.md`. Summary of scope:

### Identity Changes

| Field | Old | New |
|-------|-----|-----|
| Product Name | Hermes iOS / HermesMobile | HERALD |
| Bundle ID | com.herald.legacy | net.fihonline.herald |
| Xcode Project | HermesMobile.xcodeproj | Herald.xcodeproj |
| Xcode Scheme | HermesMobile | Herald |
| App Target | HermesMobile | Herald |
| Module Name | HermesMobile | Herald |
| GitHub Repo | fireishott/Hermes-iOS | fireishott/Herald |
| Relay Container | hermes-relay | herald-relay |
| Connector Package | hermes_mobile_connector | herald_connector |

### Code Changes

- Rename `HermesMobile.xcodeproj` → `Herald.xcodeproj`; update all `project.pbxproj` references
- `PRODUCT_BUNDLE_IDENTIFIER` → `net.fihonline.herald`
- `PRODUCT_MODULE_NAME` → `Herald`
- `MARKETING_VERSION` + `CFBundleShortVersionString` → `1.0.0`; `CURRENT_PROJECT_VERSION` → `1`
- Rename source directory `HermesMobile/` → `Herald/`
- Rename key types (see rebrand-instructions.md §3.3 for full table):
  - `HermesAvatar` → `HeraldAvatar`, `HermesHostStore` → `HeraldHostStore`, etc.
- Rename entitlements file: `HermesMobile.entitlements` → `Herald.entitlements`
- Theme preset: `.nous` → `.herald`, label `"Nous"` → `"Herald"` in `Theme.swift` / `ThemeManager.swift`
- Update `Info.plist`: `CFBundleDisplayName` + `CFBundleName` → `Herald`
- String literals: all user-visible "Hermes" → "Herald" (display names, error text, notification titles, log messages)

### Relay Changes

- `docker-compose.yml` container name: `hermes-relay` → `herald-relay`
- `.env`: `APNS_BUNDLE_ID=net.fihonline.herald`
- `relay/app/`: update user-facing strings; rename class/function references in `hermes_adapter.py`

### Connector Changes

- `pyproject.toml` name: `hermes_mobile_connector` → `herald_connector`
- Source directory: `src/hermes_mobile_connector/` → `src/herald_connector/`
- Update import statements across all Python files

### Brand Assets

Assets provided in `~/Desktop/herald-rebrand/`:
- `app-icon.png` (1254×1254 PNG) → resize to 1024×1024, place at `Herald/Resources/AppIcon.png`
- `brand-mark.png` (2172×724 PNG) → use as source for SVG recreation

SVGs to create in `docs/assets/`:
- `brand-mark.svg` — 600×120, flame left + "HERALD" text right
- `brand-mark-vertical.svg` — 200×240, flame over text
- `app-icon.svg` — 512×512, flame only
- `architecture.svg` — 800px wide, dark bg, system diagram

Color palette:
- Primary: `#FF6B00` (molten orange)
- Hot tip: `#FFF5E0` (white-hot)
- Background: `#0A0A0A`
- Surface: `#1A1D23`
- Text: `#F5F0E8`
- Accent: `#FF3D00` (ember red)

### Documentation

- `README.md` — complete rewrite per rebrand-instructions.md §6.1 (hero banner, badges, screenshots, features grid, architecture diagram, quick start, acknowledgements)
- `CHANGELOG.md` — new entry at top: `## [1.0.0] - 2026-07-19` / "Rebrand from Hermes-iOS to HERALD"
- `CONTRIBUTING.md`, `SECURITY.md`, `MAINTAINER_NOTES.md` — update name references
- New files: `docs/BUILDING.md`, `docs/CONFIGURATION.md`
- `LICENSE` — add "Copyright (c) 2026 Herald Contributors" above existing copyright line

### What NOT to Change

- Git history (preserve all commits)
- Upstream remote pointing to dylan-buck/Hermes-iOS
- License type (MIT)
- Relay API endpoints (`/v1/` prefix stays)
- Connector MCP tool names (update display names only)
- Test targets (rename but keep coverage)

---

## Part 2: Rich Chat Enhancements

### Architecture: Parse-on-Render (Option 1)

Messages are stored as raw strings (markdown + `<think>` blocks + JSON tool fences). A `ContentParser` classifies them into typed `ContentBlock` values at display time. Results are cached by message ID in the message list view. No relay or connector changes.

### 2.1 ContentBlock Model

**File:** `Herald/Core/ContentBlock.swift`

```swift
enum ContentBlock {
    case text(String)
    case code(lang: String?, body: String)
    case thinking(String)
    case toolCall(name: String, args: String?, result: String?)
    case image(URL)
    case diff(String)
    case table([[String]])
}

struct ContentParser {
    static func parse(rawText: String, toolEvents: [ToolActivity]) -> [ContentBlock]
}
```

Parser is a single-pass state machine over the raw string:
- Detects fenced code blocks (` ```lang `)
- Detects `<think>…</think>` spans
- Detects pipe-delimited tables
- Extracts diff blocks (fenced with `diff` language tag)
- Tool call blocks sourced from `toolEvents` array already tracked by `ChatStore`
- Image blocks from existing inline image URL detection

### 2.2 Message Rendering

**Refactored:** `Herald/Features/Chat/MessageBubbleView.swift`

`MessageBubbleView` renders a `VStack` of typed block views:

| Block | Renderer | Key behavior |
|-------|----------|-------------|
| `.text` | `AttributedString` | bold, italic, inline code, links |
| `.code` | `CodeBlockView` | monospace, language label, keyword-tinted syntax, horizontal scroll, copy button |
| `.thinking` | `ThinkingBlockView` | collapsed accordion by default; chevron + "Reasoning" label; streams into collapsed state live; auto-expands if user tapped before stream closed |
| `.toolCall` | `ToolCallBubbleView` | pill header "🔧 tool\_name"; expandable args/result JSON in monospace; Herald surface bg (`#1A1D23`) |
| `.diff` | `DiffBlockView` | monospace; `+` lines `#4CAF50`; `−` lines `#FF3D00` (ember) |
| `.table` | `TableBlockView` | `Grid`-based; alternating row tints |
| `.image` | existing `ImageBlockView` | integrated into block pipeline |

**Syntax highlighting:** Lightweight keyword tokenizer (no third-party dependency). Switch over ~150 keywords per language for: Swift, Python, Bash, JSON, SQL, JavaScript, TypeScript. Unrecognized language = no highlighting, monospace only.

**New renderer files:**
```
Herald/Features/Chat/Renderers/
  CodeBlockView.swift
  ThinkingBlockView.swift
  ToolCallBubbleView.swift
  DiffBlockView.swift
  TableBlockView.swift
```

### 2.3 Long-Press Message Interactions

`.contextMenu` on every `MessageBubbleView`:

| Action | Visibility | Behavior |
|--------|-----------|----------|
| Copy text | Always | Raw text to clipboard |
| Copy code | If ≥1 `.code` block | First code block body to clipboard |
| Open in Canvas | If code/markdown block exists | Push to `CanvasStore.activeArtifact` |
| Retry | Assistant messages only | Re-submit preceding user message |
| Delete | Always | Client-side removal; relay retains history |
| Share | Always | iOS share sheet with formatted text |

Standard SwiftUI `.contextMenu { }`. No third-party libraries.

### 2.4 Canvas Panel

**Model:** `Herald/Models/Artifact.swift`

```swift
struct Artifact: Codable, Identifiable {
    var id: UUID
    var sessionID: String
    var type: ArtifactType   // .code(lang: String), .markdown, .svg
    var content: String
    var updatedAt: Date
}
```

Persisted to `UserDefaults` keyed by sessionID. No relay changes.

**`CanvasStore`** (`@Observable`): `Herald/Features/Canvas/CanvasStore.swift`
- Holds `activeArtifact: Artifact?`
- `open(block: ContentBlock, sessionID: String)` — converts a `.code` or `.text` block into an `Artifact` and sets it as active
- `clear()` — dismisses canvas
- Auto-population heuristic: if an assistant message is a single `.code` block >15 lines with no other block types, push it automatically

**iPad:** New `Canvas` tab in `iPadRightPanelView`. `CanvasView` with:
- Toolbar: language label, copy button, close button
- `TextEditor` body (editable)
- Persists edits back to `CanvasStore`

**iPhone:** Canvas icon added to chat toolbar → `.sheet` presenting `CanvasView`.

**New files:**
```
Herald/Features/Canvas/
  CanvasView.swift
  CanvasStore.swift
Herald/Models/Artifact.swift
```

**Modified files:**
- `Herald/Features/Chat/ChatScreen.swift` — canvas toolbar button (iPhone)
- `Herald/Features/Sidebar/iPadRightPanelView.swift` — Canvas tab

---

## Part 3: Retry / Non-Response Bug

`a6b39e1` (2026-07-19) added per-job deadline in the connector WebSocket loop and `773e84d` added client-side watchdog + auto-retry. If silent failures continue after this lands in production, open a separate `superpowers:systematic-debugging` pass with live relay logs as primary evidence. Not a blocking design question for this release.

---

## Verification Checklist

### HERALD Rebrand
- [ ] `xcodebuild build` succeeds with `Herald.xcodeproj` / `Herald` scheme
- [ ] App displays "Herald" on home screen
- [ ] Bundle ID is `net.fihonline.herald`
- [ ] No "Hermes" text visible in the UI (settings, about, errors)
- [ ] Relay starts as `herald-relay` container
- [ ] Connector connects successfully
- [ ] README renders correctly on GitHub with new branding
- [ ] LICENSE shows both copyright lines
- [ ] Fork relationship to upstream preserved

### Rich Chat
- [ ] Markdown renders correctly in assistant messages (bold, italic, inline code, links)
- [ ] Code blocks show language label, copy button, and syntax tinting
- [ ] Long reasoning responses show collapsed "Reasoning" accordion
- [ ] Tool call events render as distinct pill-header bubbles
- [ ] Long-press on a message shows all applicable context menu items
- [ ] "Open in Canvas" pushes code block to canvas panel
- [ ] Canvas is editable and persists across message sends in the same session
- [ ] iPhone canvas opens as a sheet from toolbar icon
- [ ] iPad canvas appears in right-panel Canvas tab
- [ ] Streaming: thinking block accumulates in real-time without layout thrash
