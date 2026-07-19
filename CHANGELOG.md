# Changelog

All notable changes to Hermes iOS are documented here.

## [1.1.0] - 2026-07-19

### Added - Mimo TTS + Bug Fixes

- **Mimo TTS Integration** (`Services/Live/MimoTTSService.swift`): Full text-to-speech via Xiaomi MiMo v2.5 TTS API. Uses OpenAI-compatible chat completions format (`POST /v1/chat/completions` with model `mimo-v2.5-tts`). Returns base64-encoded WAV audio, played back via AVAudioPlayer.

- **TTSServiceProtocol** (`Services/Protocols/TTSServiceProtocol.swift`): Protocol for TTS services with `synthesize()`, `speak()`, `stop()`, and `isPlaying` state.

- **8 Premium Voices**: Mia (lively girl), Chloe (sweet dreamy), Milo (sunny boy), Dean (steady gentleman) for English. 冰糖, 茉莉, 苏打, 白桦 for Chinese.

- **Voice Settings** (Settings → Voice): Mimo API key field (stored in UserDefaults), voice picker dropdown, TTS on/off toggle, auto-speak toggle for Talk mode.

- **Read-Aloud Buttons**: Speaker icon on Hermes chat messages and Talk transcript bubbles to read any response aloud via Mimo TTS.

- **Auto-Speak in Talk**: When enabled, completed Herald responses in Talk mode are automatically spoken aloud via Mimo TTS.

- **iPhone Permissions Fix**: Permissions screen now uses NavigationLink inside the settings sheet instead of dismissing and pushing onto the chat NavigationStack. Fixes the issue where permissions appeared behind the settings sheet on iPhone.

### Changed

- TalkStore now has `ttsService`, `ttsSettingsProvider`, `speakText()`, `stopTTS()`, and `autoSpeakLatestHermesResponse()` for TTS integration.
- UserSettings gained `ttsEnabled`, `ttsVoice`, `ttsAutoSpeak` properties with backward-compatible Codable migration.
- AppContainer wires MimoTTSService into TalkStore at launch.
- Version badge updated to 1.1.0.
- README updated with Mimo TTS feature documentation.

## [1.0.0] - 2026-07-19

### Changed
- Rebrand from Hermes-iOS to HERALD
- New bundle ID: `net.fihonline.herald`
- New relay container: `herald-relay`
- New connector package: `herald-connector`
- Theme preset renamed: `.nous` → `.herald` with brand orange (#FF6B00) accent
- Version reset to 1.0.0 to mark the new identity

## [0.10.0] - 2026-07-17

### Added - Session Management + Right Panel + iPhone Drawer

- **Session Management API** (`relay/app/main.py`, `relay/app/services.py`): 9 new REST endpoints for full session lifecycle — list, search, create, delete, archive, toggle pin, rename, load conversation. Paginated with `limit`/`offset`.

- **Device-scoped sessions** (`relay/app/models.py`): Added `device_id`, `source`, `is_pinned`, `preview_text` columns to Conversation. Sessions created from an iPhone only appear on that iPhone; sessions from iPad only appear on that iPad. Hermes-host sessions (CLI, Telegram, etc.) with null `device_id` are visible across all devices.

- **iPhoneSessionDrawer** (`Features/Sidebar/iPhoneSessionDrawer.swift`): Slide-out session browser for iPhone. Drag-from-left-edge gesture with spring animation, hamburger button in chat toolbar. Shows pinned/recent sessions, context menu for pin/archive/delete, load-more pagination.

- **iPadRightPanelView** (`Features/Sidebar/iPadRightPanelView.swift`): Right-side inspector panel with three tabs — Logs (scrollable log feed with level filters), Terminal (console-style output), Tools (token usage). Toggle via `sidebar.right` button in sidebar header and detail toolbar.

- **Session browser (iPad sidebar)**: Full session list in sidebar with search, pinned section, recent section, platform sub-sections, swipe actions, context menu for rename/pin/archive/delete.

### Changed

- **Conversation scoping**: `get_or_create_current_conversation` and `archive_current_conversation` now accept optional `device_id`. Current conversation is device-scoped, preventing cross-device session leakage.
- **ChatScreen**: Accepts `$isSessionDrawerOpen` binding for iPhone drawer toggle. Hamburger button added to leading toolbar on iPhone.
- **MainTabView**: Wraps content in ZStack with `iPhoneSessionDrawer` overlay.
- **AdaptiveRootView**: iPad layout now uses HStack with NavigationSplitView + right panel. Right panel toggle in detail toolbar.
- **iPadSidebarView**: Header now has new-chat button + right panel toggle.

### Relay API Surface

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/v1/sessions` | List sessions (paginated) |
| GET | `/v1/sessions/search?q=` | Search by title |
| POST | `/v1/sessions` | Create new session |
| GET | `/v1/sessions/{id}` | Get session summary |
| GET | `/v1/sessions/{id}/conversation` | Load full conversation |
| DELETE | `/v1/sessions/{id}` | Delete session |
| POST | `/v1/sessions/{id}/archive` | Archive session |
| POST | `/v1/sessions/{id}/pin` | Toggle pin |
| PATCH | `/v1/sessions/{id}` | Rename session |

### Files Changed

| File | Status |
|------|--------|
| relay/app/models.py | Modified |
| relay/app/services.py | Modified |
| relay/app/main.py | Modified |
| Features/Sidebar/iPhoneSessionDrawer.swift | New |
| Features/Sidebar/iPadRightPanelView.swift | New |
| Features/Sidebar/AdaptiveRootView.swift | Modified |
| Features/Sidebar/iPadSidebarView.swift | Modified |
| ContentView.swift | Modified |
| Features/Chat/ChatScreen.swift | Modified |
| HermesMobile.xcodeproj/project.pbxproj | Modified |

### Added - iPad Layout Support

- **DeviceClass** (`Core/DeviceClass.swift`): enum detecting iPad vs iPhone at runtime via UIDevice.current.userInterfaceIdiom. Static current, isPad, isPhone properties (MainActor-isolated).

- **SidebarSection** (`Features/Sidebar/iPadSidebarView.swift`): enum with .chat, .inbox, .talk, .settings cases. Each has title and icon computed properties.

- **iPadSidebarView**: SwiftUI sidebar using List with .sidebar style. Shows section icons, titles, and an orange offline dot next to Chat when hostStore.connectionState != .online. Uses Design system tokens throughout.

- **AdaptiveRootView** (`Features/Sidebar/AdaptiveRootView.swift`): Root view that branches between NavigationSplitView (iPad, sidebar + detail) and MainTabView (iPhone, existing tab bar). Driven by DeviceClass.isPad.

- **TabRouter iPad extension** (`Core/Router.swift`): Added SidebarSection enum and oniPadSectionSwitch callback closure. When .settings is presented on iPad, calls the callback instead of presenting a sheet. All existing iPhone behavior unchanged.

### Changed

- **AppRootView**: Now renders AdaptiveRootView instead of MainTabView directly.
- **project.pbxproj**: New files registered in the main HermesMobile target Sources build phase.

### Architecture

iPad layout uses NavigationSplitView:

```
+--------------+----------------------------+
| Sidebar      | Detail                     |
| (list)       |                            |
|   Chat  <--  | ChatScreen / InboxScreen / |
|   Inbox      | TalkModeScreen /           |
|   Talk       | SettingsScreen             |
|   Settings   |                            |
+--------------+----------------------------+
```

iPhone layout unchanged: MainTabView with existing tab bar.

### Files Changed

| File | Status |
|------|--------|
| Core/DeviceClass.swift | New |
| Features/Sidebar/iPadSidebarView.swift | New |
| Features/Sidebar/AdaptiveRootView.swift | New |
| Core/Router.swift | Modified |
| Features/Onboarding/AppRootView.swift | Modified |
| HermesMobile.xcodeproj/project.pbxproj | Modified |
