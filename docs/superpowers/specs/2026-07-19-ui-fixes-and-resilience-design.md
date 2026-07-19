# UI Fixes, Job Retry Resilience, Session Auto-Titling, Live Logging

## Overview

Nine fixes surfaced from real device testing after the themes/wallpaper and model-switching rounds shipped. Two investigation agents traced each report to a precise root cause before any fix was proposed — see each section for citations.

**Explicitly out of scope for this batch** (deferred to their own brainstorm/design passes):
- Rich Chat collaboration mode (markdown, inline files/artifacts, persistent canvas, structured multi-turn blocks)
- Configurable TTS models for Talk mode

**Confirmed NOT a bug** (verified live against the running ignyte host, not just read in source):
- Reasoning/streaming text display — every message send during live testing (7:38, 7:40, 7:41, 7:42, 7:43, 7:44 AM) completed with `POST /v1/chat/completions 200 OK`, and the SSE stream from the Hermes API server correctly emits `reasoning_content` and `content` deltas in the OpenRouter/vLLM convention. Neither the themes round nor the model-switching round touched any code in this pipeline (confirmed via `git log` on the relevant files). This item was removed from the plan.

---

## Section 1: Quick UI Fixes (iOS)

### 1.1 Keyboard doesn't auto-dismiss after sending

**Root cause:** `ChatScreen.swift` has `@FocusState private var isComposerFocused: Bool`, set to `false` in exactly one place (a tap-outside gesture on the message list). `sendMessage()` never touches it.

**Fix:** Set `isComposerFocused = false` at the top of `ChatScreen.sendMessage()`.

### 1.2 Enter key doesn't send message

**Root cause:** `ChatInputBar.swift` uses `TextField(..., text: $text, axis: .vertical)` with `.lineLimit(1...5)` and a correctly-wired `.onSubmit { handlePrimaryAction() }`. The wiring is correct, but `axis: .vertical` growable TextFields have a long-standing SwiftUI behavior where the Return key inserts a newline instead of firing `.onSubmit`.

**Fix:** Intercept the Return key explicitly rather than relying on `.onSubmit`. Use `.onKeyPress(.return)` (iOS 17+) on the text field to call `handlePrimaryAction()` and consume the event, preventing the newline insert. Preserve the ability to type actual newlines via Shift+Return (`.onKeyPress` can distinguish modifiers) so multi-line composition still works.

### 1.3 iPad sidebar: no way back to chat

**Root cause:** `SidebarSection` enum defines a `.chat` case, but `iPadSidebarView.swift`'s `bottomSections` only iterates `[SidebarSection.inbox, .talk, .settings]` — `.chat` is deliberately excluded. The only way to reach `.chat` is via `sessionRow(_:)`, which is itself hidden once `selectedSection != .chat` (the session list block is gated on `selectedSection == .chat`).

**Fix:** Add `.chat` to the `bottomSections` iteration list: `[SidebarSection.chat, .inbox, .talk, .settings]`, so it's always a reachable row regardless of current section.

### 1.4 iPhone sidebar needs swipe-to-open gesture

**Root cause:** `iPhoneSessionDrawer.swift` has a `DragGesture` correctly attached to an `HStack` containing the drawer box and a `Spacer()`. When closed, the drawer box (the only painted/backgrounded content) is offset off-screen (`-drawerWidth + dragOffset`), leaving only the transparent `Spacer()` on-screen. SwiftUI does not hit-test fully transparent, backgroundless regions without an explicit `.contentShape()`.

**Fix:** Add `.contentShape(Rectangle())` to the `HStack` (or add a dedicated `Color.clear.contentShape(Rectangle())` edge-catcher strip along the leading screen edge) so the gesture has a real hit-testable surface even when the drawer is visually off-screen.

### 1.5 New session button doesn't work

**Root cause (two compounding bugs):**
- `SessionListStore.createNewSession(title:)` never sets `selectedSection = .chat` on success, unlike `sessionRow` which does. If pressed from Inbox/Talk/Settings, the session is created and switched server-side but the visible detail pane doesn't change — looks like nothing happened.
- `errorMessage` is set on failure (in both `createNewSession` and `switchToSession`) but is never read/displayed anywhere in `iPadSidebarView.swift` or `iPhoneSessionDrawer.swift`. A genuine API failure fails completely silently.

**Fix:**
- In `iPadSidebarView.swift`'s "new chat" button action, after `await sessionStore.createNewSession()` succeeds, set `selectedSection = .chat`.
- Add an inline error banner (or `.alert`) in both `iPadSidebarView.swift` and `iPhoneSessionDrawer.swift` bound to `sessionStore.errorMessage`, dismissible, shown whenever it's non-nil.

### 1.6 Profile Selector not visible

**Root cause:** `ChatScreen.swift`'s `profileChip` is gated on `profileStore.activeProfile != nil`. `ProfileStore.activeProfile` requires the connector's `profiles.list` RPC to report a non-nil `activeProfileName`, which in turn requires `~/.hermes/profiles/<profile>/config.yaml`'s `profile.default` key to be explicitly set on the host. If no default profile has ever been configured, `profiles` can be non-empty while `activeProfile` stays permanently `nil` — hiding the very UI meant to let the user pick one.

**Fix:** Change the chip's visibility gate from `profileStore.activeProfile != nil` to `!profileStore.profiles.isEmpty`. When profiles exist but none is active, show a neutral "Select Profile" label instead of a profile name.

---

## Section 2: Job Retry Resilience (iOS + Relay)

**Root cause:** Confirmed live — a message sent during testing produced zero `POST /v1/chat/completions` calls for a full 60-second window (07:34:51 to 07:35:50), while RPC catalog traffic continued normally. The job was dropped somewhere between the relay claiming it and the connector dispatching it to the Hermes API server, with no server-side error logged. This is intermittent and not reliably reproducible from reading code alone.

**iOS side (`ChatStore`):**
- On job submission, start a client-side watchdog `Task` racing the SSE stream (e.g., 30s timeout)
- If no `job.progress` or `done` event arrives before the watchdog fires, automatically retry once — re-POST the same message — without requiring user action
- Track a retry count per pending message (max 1 automatic retry)
- If the retry also stalls or fails, replace the current bare "Retry" icon with an inline error state showing real text ("Hermes didn't respond — tap to retry") that the user can tap to manually retry again

**Relay side (`relay/app/main.py` or a background sweep in `services.py`):**
- Add a `logger.warning(...)` when a `MessageJob` is found still in `queued` status past its `lease_expires_at` without being claimed. This doesn't fix the root cause (the drop is likely a WebSocket-layer race between the relay and connector) but makes future occurrences diagnosable via relay logs instead of silently invisible.

---

## Section 3: Session Auto-Titling (Relay)

**Root cause:** `relay/app/models.py` sets `title: Mapped[str] = mapped_column(Text, nullable=False, default="Hermes")`, and `get_or_create_current_conversation()` in `services.py` explicitly creates every auto-managed "current" conversation with `title="Hermes"`. There is no auto-title-from-first-message logic anywhere in the relay.

**Fix:** In `relay/app/services.py`, in the message-append path (`append_message` or wherever the first user message lands on a conversation), check if the conversation's title is still the default `"Hermes"` placeholder. If so, derive a title by truncating the first user message to ~40 characters (strip newlines, collapse whitespace, append `…` if truncated) and update the conversation's title. This only fires once per conversation — subsequent messages don't re-derive the title, and manually-renamed sessions (title != "Hermes") are never touched.

No LLM call — plain truncation only, to avoid adding latency or cost to every first message in a conversation.

---

## Section 4: Live Logging (iOS — cherry-pick, not new work)

**Root cause:** `iPadRightPanelView.swift`'s Logs tab reads a purely local `@State private var logEntries: [LogEntry] = []` that nothing ever appends to — it's a permanently-empty stub on `master`. A real implementation already exists on the unmerged branch `feat/ipad-layout` (commits `f20df34`, `8ae88eb`), which adds `logEntries`/`appendLog(level:_:)` directly on `ChatStore`, called during the SSE streaming lifecycle ("Streaming started", "Message accepted — job …").

**Fix:** Cherry-pick `f20df34` and `8ae88eb` onto `master`. Reconcile the cherry-pick with:
- Our `@MainActor` annotation on `LogLevel.color` (added during the themes round to match adaptive `Design.Colors`) — should merge cleanly since it's an isolated addition that doesn't touch `logEntries` wiring.
- Rewire `iPadRightPanelView.swift`'s Logs tab to read `chatStore.logEntries` instead of the local stub, matching what the branch does.

**Scope confirmation:** this is a client-side activity log narrating the app's own streaming lifecycle, not a real tail of host/agent process logs. The Terminal tab's "Terminal integration coming soon." placeholder stays as-is — out of scope for this batch.

---

## Implementation Order

1. **Quick UI fixes (Section 1)** — six independent, low-risk fixes, can be done in any order or batched
2. **Session auto-titling (Section 3)** — relay-only, independent of iOS work
3. **Live logging cherry-pick (Section 4)** — should land before/independent of the other iOS changes to minimize merge conflict surface
4. **Job retry resilience (Section 2)** — touches `ChatStore` most heavily, do last so it doesn't conflict with the live-logging cherry-pick's `ChatStore` changes

## Key Patterns

- **iOS:** Follow existing `@FocusState`/`@State`/`ChatStore` patterns already established in the file — no new architecture needed for any of these fixes
- **Relay:** Title derivation and job-staleness logging both follow existing `logger.warning(...)` / plain-function patterns already used throughout `services.py`
- **Cherry-pick:** Verify build after merge — the branch predates several master commits (session management, theme system) so conflicts are possible in `iPadRightPanelView.swift` and `ChatStore.swift`
