# Changelog

All notable Kallisti changes are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.2.1] - 2026-08-09

### Fixed

- WebSocket frames over 1 MB no longer kill the connection. Image-generation
  tool results routinely exceed 2-4 MB as base64, and the previous 1 MB
  receive cap silently dropped the terminal event, leaving the app stuck on
  "Thinking" until the stall watchdog fired and the turn was re-submitted
  (double billing). The client now accepts frames up to 64 MB.
- Long-running tool calls no longer trip the stall watchdog. The watchdog
  skips its check while a tool is in flight, so image generation and other
  multi-minute tool executions complete without a false "took too long" and
  without re-running the same job.
- Push registration, gateway logs, restart, and live activity registration
  now refresh the native gateway bearer before every request, eliminating
  401s caused by expired stored tokens.
- The loading surface no longer tears down mid-session on reconnect. A
  `hasConnectedOnce` flag keeps the chat UI mounted during recovery, so typed
  text and scroll position survive connection churn.
- Ping timeout raised to 45 seconds so a stalled gateway event loop during a
  heavy turn cannot kill a healthy socket.
- The model pill reloads from the gateway on connect, so it no longer shows a
  bare green dot until manually opened.
- iPad model pill layout fixed: the model name text now has layout priority
  and renders instead of being compressed to zero width.
- Live Activity lockscreen status now includes an elapsed-time heartbeat, so
  the lockscreen no longer freezes on "Thinking".
- Opening a previous session falls back to `session.resume` before creating a
  blank chat, restoring the correct transcript after gateway restarts.

### Added

- Tool-in-flight watchdog exemption with an absolute job deadline safety net.
- Native bearer token priority for all connector-facing HTTP calls.
- Speech recognition permission included in onboarding permission requests.

## [0.2.0-build.50] - 2026-08-09

### Added

- Full-screen branded connection overlay showing real-time connection stages during connect, reconnect, and relaunch with stored login.
- Manual Reset Connection action from the overlay (after a short delay) and from Settings, for tearing down stale transport and starting a fresh authenticated connection.
- Historical MEDIA references are resolved after conversation reload so images remain accessible after gateway restarts.
- Media path normalization: client converts absolute server paths to relative forms; connector resolves both current default-profile and legacy per-profile image roots.
- Focused regression tests for connection stage transitions, overlay visibility and error suppression, reset idempotency, and media path backward compatibility.

### Fixed

- Reconnect and relaunch states no longer flash the raw "Cannot connect to gateway" error while recovery is in progress.
- Stale connection state is properly torn down on manual reset without overlapping reconnect loops.
- Stored credentials and conversations are preserved through manual connection reset.

## [0.2.0-build.49] - 2026-08-09

### Added

- Native `MEDIA:` path conversion to authenticated inline-image Markdown.
- Restricted `/v1/native/media` connector endpoint for agent-generated images.
- Native image upload staging through `image.attach_bytes` before prompt submission.
- Regression coverage for inline media, image staging, fast acknowledgments, stalled streams, and pre-ack placeholder ownership.

### Fixed

- Follow-up messages no longer wait behind a stream that ended after acknowledgment without a terminal event.
- Pre-ack transcript polling no longer removes or duplicates the active thinking placeholder.
- Accepted outbox jobs settle through deadline-aware recovery and release the per-conversation FIFO lease.
- Native gateway responses arriving during WebSocket send registration are no longer dropped.
- Public media URLs use the configured HTTPS gateway host instead of an unreachable private connector port.
- Authenticated image requests attach credentials for `/v1/native/` routes without leaking them to arbitrary image hosts.
- Connector media downloads accept validated gateway cookie sessions, native bearer tokens, and persisted paired-device tokens.
- Connector restarts rehydrate paired-device tokens and preserve native-watch refresh credentials.
- Push registration and Live Activity cleanup no longer block chat startup or leave stale activity state.

### Security

- Media serving is limited to configured Hermes image/media roots.
- Unsupported file types, traversal attempts, missing files, and files larger than 10 MB are rejected.
- Gateway cookie and bearer validation is delegated to the gateway's authenticated identity endpoint.
- Public source and documentation were scrubbed of personal names, user-specific paths, private host addresses, device identifiers, captured production logs, and signing credentials.

## [0.2.0-build.41] - 2026-08-09

### Added

- One-time Kallisti pairing-code sign-in for native gateway mode.
- Direct native WebSocket transport for chat, session history, model selection, and profile selection.
- Gateway status, logs, restart operations, push notifications, and Live Activities.

### Fixed

- WebSocket send timeouts and response-registration races.
- Session identity and reconnect handling.
- Model and profile selection against the active session.
- Native basic-auth and pairing-cookie login flows.
- Gateway ticket refresh and session creation timeouts.

## [0.1.0] - 2026-08-03

### Added

- Initial Kallisti branding and iOS application structure.
- Connector, relay, widget, watch, intents, and notification-extension targets.

### Attribution

- Built from the earlier Herald mobile client foundation under the repository's MIT license history.
