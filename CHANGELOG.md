# Changelog

All notable changes to Kallisti will be documented in this file.

## [0.2.0] - 2026-08-09

### Added
- One-time Kallisti pairing-code sign-in for native gateway mode, bound to this device installation

### Fixed
- Pairing now creates the authenticated gateway session before opening Kallisti and uses it to mint WebSocket tickets

## [0.2.0] - 2026-08-09

### Fixed
- Model picker now reads the active conversation model state instead of overwriting it with the global default

## [0.2.0] - 2026-08-09

### Fixed
- Native gateway sign-in now uses the self-hosted basic-auth session instead of the retired Nous OAuth provider
- Kallisti obtains fresh WebSocket tickets from the authenticated HttpOnly session without storing the gateway password

## [0.2.0] - 2026-08-08

### Added
- Native gateway mode: direct WebSocket JSON-RPC to your Hermes gateway, no relay/connector pairing required
- Model picker with live catalog from the gateway (`model.options`), dedupe for keyed providers
- Profile selector over the gateway (native mode) and relay (legacy mode)
- Auxiliary model overrides (vision, web extract, compression) surfaced from gateway config
- Push notifications with dynamic APNs environment detection (development vs production)
- Live Activities / Dynamic Island support with corrected APNs bundle
- Session resume: stable session identity across reconnects (idMap registration)

### Fixed
- Session list: cron/internal sessions no longer pollute history (source filter)
- Delete/rename/follow-up on resumed sessions no longer fail with "session not found" - session UUIDs are registered on load
- Follow-up latency: resumed sessions keep their server session instead of cold-starting a new one
- Model switch now scopes to the active session (session_id passed to slash.exec)
- Settings status: host row reflects live gateway connectivity, not relay token presence
- Restart/logs/update controls show an honest "not available in direct gateway mode" state instead of a confusing pairing error
- Gateway logs work in native mode via `hermes logs` over the gateway socket
- Settings infrastructure: provider count and model decode from gateway config (list[dict] not list[string])
- WebSocket transport sends TEXT frames (binary frames were dropped by the gateway)
- OAuth via ASWebAuthenticationSession (RFC 8252) - no cookie drops on the Nous redirect chain
- Session list decode: `started_at` number vs string, `id` key alias tolerance

### Changed
- Complete rebrand: Herald → Kallisti
- New icon: woodcut relief print seal mark
- New theme: obsidian/platinum/pewter/steel, Playfair Display headlines
- Bundle ID: net.fihonline.kallisti
- Version scheme: 0.x.x (matching MARKETING_VERSION)

## [0.1.0] - 2026-08-03

### Added
- Brand asset package (icons, marks, splash, OG image)
- Landing page with full brand identity
- Rebrand instructions for Claude Code execution

### Changed
- Complete rebrand: Herald → Kallisti
- New color palette: obsidian/platinum/pewter/steel
- Tagline: "To the Most Beautiful."
- Website: kallisti.app / kallisti.fihonline.net

### Attribution
- Built on the foundation of [Herald](https://github.com/fireishott/Herald)
- Original work licensed under MIT

## Herald (pre-rebrand)

- Herald gateway and iOS companion app
- Original identity and branding
