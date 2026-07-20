# Changelog

All notable changes to Hermes iOS are documented here.

## [1.2.7] - 2026-07-20

### Changed - iPad Three-Panel Layout

- **Adaptive root mounted** (`Herald/Features/Onboarding/AppRootView.swift`): `AppRootView` now renders `AdaptiveRootView()` after onboarding instead of `MainTabView()`. `AdaptiveRootView` is responsible for choosing `MainTabView` on iPhone.

- **Three-column layout** (`Herald/Features/Sidebar/AdaptiveRootView.swift`): iPad now uses a proper three-column `NavigationSplitView` with sidebar, content, and detail columns. The right panel is no longer an overlay — it's a genuine detail column.

- **Router synchronization** (`Herald/Features/Sidebar/AdaptiveRootView.swift`): `oniPadSectionSwitch` binding is installed/removed with the adaptive view lifecycle. Router tab changes are synchronized with sidebar section selection.

- **Panel placeholder**: When the detail panel is closed, a placeholder view is shown instead of empty space.

- **Accessibility**: Added accessibility label to the detail panel toggle button.

## [1.2.6] - 2026-07-20

### Changed - iPhone Toolbar Cleanup

- **Separate phone/pad toolbars** (`Herald/Features/Chat/ChatScreen.swift`): Toolbar now uses conditional compositions via `DeviceClass.isPhone` instead of trying to fit all controls into one layout.

- **iPhone leading**: Hamburger/session drawer button only, with accessibility label.

- **iPhone principal**: New `compactStatusControl` showing connection dot + compact model name + context ring. Width-bounded (no `fixedSize`) to prevent system overflow ellipsis.

- **iPhone trailing**: Canvas only. Removed duplicate Settings gear (iPhone has Settings tab in bottom tab bar).

- **iPad**: Retains richer profile/model/timer presentation. Settings gear remains since iPad has no tab bar.

- **Accessibility**: Added accessibility labels to all icon-only toolbar controls.

## [1.2.5] - 2026-07-20

### Added - Lock-Screen Notification Actions

- **Notification categories** (`Herald/Services/Protocols/NotificationServiceProtocol.swift`, `Herald/Services/Live/LiveNotificationService.swift`): Registered `HERALD_MESSAGE_READY` (Read, Reply, Nudge) and `HERALD_JOB_ACTIVE` (Read, Stop, Nudge) categories with stable action identifiers.

- **Reply action** (`Herald/Stores/AppContainer.swift`): `UNTextInputNotificationAction` sends typed text to the notification's `conversationId` using a fresh `clientMessageId`. Works regardless of currently displayed conversation.

- **Nudge action** (`Herald/Stores/AppContainer.swift`): Sends fixed follow-up text "Continue, and give me a concise status update." to the correct conversation.

- **Stop action** (`Herald/Stores/AppContainer.swift`, `relay/app/main.py`, `connector/src/herald_connector/client.py`): Cancels `jobId` end to end via new `POST /v1/jobs/{job_id}/cancel` endpoint and `jobs.cancel` connector RPC. Idempotent for already-completed jobs.

- **Job action handling** (`Herald/AppEntry.swift`): Notification handler extracts reply text from `UNTextInputNotificationResponse` and delegates to `AppContainer`.

- **Relay cancel endpoint** (`relay/app/main.py`): New `POST /v1/jobs/{job_id}/cancel` verifies job ownership, dispatches connector RPC for running jobs, and publishes terminal `cancelled` SSE event.

- **Connector cancel RPC** (`connector/src/herald_connector/client.py`): New `jobs.cancel` RPC method cancels the running asyncio task, cleans up staged attachments, and returns `{jobId, status: "cancelled"}`.

## [1.2.4] - 2026-07-20

### Fixed - Notification Metadata + Crash-Safe Routing

- **Push broker metadata** (`relay/app/schemas.py`, `relay/app/main.py`): Push broker request now carries `conversationId`, `messageId`, `jobId`, and `category` fields through the signed payload. Both direct APNs and managed broker transport produce identical notification payloads.

- **Notification category** (`relay/app/main.py`): Message completion pushes now use `HERALD_MESSAGE_READY` category identifier for lock-screen action support.

- **Crash-safe notification routing** (`Herald/Stores/AppContainer.swift`): New `handleNotificationRoute` method processes notification taps through a single entry point. Pending routes are stored during cold launch and processed exactly once after initialization.

- **Direct load-by-ID** (`Herald/AppEntry.swift`): Notification handler now extracts primitive strings from `UNNotificationResponse` and delegates to `AppContainer`. The no-argument `loadConversation()` fallback is removed — notifications either load the exact conversation by ID or show a recoverable error.

- **Single-flight initialization** (`Herald/Stores/AppContainer.swift`): `initialize()` now guards against concurrent callers to prevent competing state writers during cold launch.

- **Push broker test updated** (`relay/tests/test_push_broker.py`): Test now asserts `category: "HERALD_MESSAGE_READY"` and metadata fields (`conversationId`, `messageId`, `jobId`) are forwarded to APNs.

## [1.2.3] - 2026-07-20

### Changed - Resumable Live Job Connection

- **Connector job heartbeats** (`connector/src/herald_connector/client.py`): Jobs now emit `job.started` immediately and `job.heartbeat` every 10 seconds with phase tracking (`starting`, `thinking`, `tool`, `writing`, `cli_waiting`). Active jobs survive across WebSocket reconnects.

- **Relay renewable lease** (`relay/app/main.py`, `relay/app/services.py`): Job lease is now renewed by `job.started`, `job.heartbeat`, and `job.progress` messages instead of using a fixed 180-second wall-clock deadline. Healthy long-running jobs are no longer killed by timeout.

- **SSE event IDs** (`relay/app/main.py`): Job events now carry monotonically increasing `eventId` fields for reconnection tracking.

- **Job status endpoint** (`relay/app/main.py`): New `GET /v1/jobs/{job_id}` returns authoritative job status for recovery after SSE gaps.

- **Grace period on disconnect** (`relay/app/main.py`): Connector WebSocket disconnect no longer immediately fails in-flight jobs. A `reconnecting` event is published and the job's lease governs recovery.

- **iOS resumable streaming** (`Herald/Services/Live/LiveHeraldClient.swift`): SSE EOF without `done` is now treated as a transport interruption, not success. The client checks job status via `GET /v1/jobs/{id}` and handles completed/failed/running states appropriately.

- **New streaming events** (`Herald/Models/StreamingUpdate.swift`): Added `.started(phase:)`, `.heartbeat(phase:)`, `.reconnecting`, and `.cancelled` cases for richer job lifecycle visibility.

- **SSE event ID parsing** (`Herald/Services/Support/RelayAPIClient.swift`, `Herald/Models/SSEEvent.swift`): SSE parser now extracts `id:` fields for reconnection tracking.

- **Polling safety net** (`Herald/Stores/ChatStore.swift`): Polling no longer marks messages as `.failed` after exhausting attempts. Server-authoritative job state is respected.

## [1.2.2] - 2026-07-20

### Fixed - Build 12 & 13 Reconciliation

- **Build 12** (`Services/Live/LiveHeraldClient.swift`): Fixed reasoning display — preserve reasoning content across metadata merge so chain-of-thought text survives conversation refresh.
- **Build 13** (`AppEntry.swift`): Fixed Swift 6 strict concurrency for `UNUserNotificationCenterDelegate` methods — notification delegate data crossings now use primitive `Sendable` types only.

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
