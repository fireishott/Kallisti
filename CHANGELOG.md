# Changelog

All notable changes to Hermes iOS are documented here.

## [1.2.1] - 2026-07-20

### Fixed - Streaming watchdog, scroll, iPad swipe, /new command, MCP

- **Bug B — False "tap to retry" during multi-tool/multiagent work** (connector + relay + app): The streaming pipeline went silent during tool execution and subagent fan-out because the connector only parsed `delta.content` and ignored `delta.tool_calls`. A 120s client watchdog misfired on that silence, showing "Herald didn't respond" while Hermes was still working.
  - `connector/src/herald_connector/herald_api_executor.py`: Now handles `delta.tool_calls` — emits `tool_activity` StreamEvent for each tool function name. Also emits `keepalive` events when the upstream SSE sends a chunk with no user-visible content (role-only deltas, empty content during subagent work).
  - `connector/src/herald_connector/client.py`: Forwards `keepalive` events to the relay as `job.progress` with `kind: "keepalive"`.
  - `Herald/Models/StreamingUpdate.swift`: New `.keepalive` case.
  - `Herald/Services/Live/LiveHeraldClient.swift`: Handles `"keepalive"` SSE events from the relay.
  - `Herald/Stores/ChatStore.swift`: `.keepalive` resets the watchdog timer via `progressContinuation`. `failStalledMessage` now preserves the placeholder ID so a late `.finished` can find and replace the error message with the actual response. Post-failure polling task refreshes the conversation after 15s to pick up server-side completion.

- **Bug A — Sent message not scrolled into view** (`ChatScreen.swift`): Removed the `streamingMessageID == nil` guard from the `pendingMessageSentAt` onChange handler. User messages now always scroll into view on send, even when streaming starts immediately.

- **Bug C — iPad swipe for logs panel** (`AdaptiveRootView.swift`): Added a `DragGesture` to the iPad detail content — swipe left opens the right panel (logs), swipe right closes it. Matches the existing iPhone drawer gesture pattern.

- **Bug D — `/new` starts new session without confirmation** (`ChatScreen.swift`, `SlashCommand.swift`): Split `/new` from `/clear`. `/new` now calls `performClear()` immediately without the destructive confirmation dialog. `/clear` retains the confirmation. Marked `/new` as `isDestructive: false`.

- **Bug F — MCP `hermes_mobile` stale command path** (`connector/src/herald_connector/client.py`): Added `register_native_mcp_server()` call on every connector connect, not just at enroll/setup. Self-heals stale `herald-mcp` paths in `~/.hermes/config.yaml` when the connector venv moves or is reinstalled.

### Notes

- Bug B fix requires all three components (connector + relay + app) deployed together. Ship connector changes before or with the app change so an older app safely ignores unknown `keepalive` events.
- The relay forwards arbitrary `kind` values through `publish_job_event` — no relay code change was needed for `keepalive`.

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
