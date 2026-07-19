# Connection Modes

Hermes iOS supports three first-class connection modes. Each preserves the same underlying architecture (iOS → relay HTTP/SSE → connector WebSocket → local Hermes), but differs in reachability, push delivery, and what the UX promises.

| Mode | Default? | Reachability | Official push | Honesty bar |
| --- | --- | --- | --- | --- |
| Managed Relay | When build enables it | Public hosted | Yes, via App Attest push broker | "Can wake the app in background" |
| Self-Hosted Tailscale | No | Tailnet-only | No | "Foreground / tailnet-connected only" |
| Self-Hosted Relay URL | Default when no managed build | User's public URL | No (unless relay opts into the broker) | "Background delivery depends on your relay" |

## Managed Relay

Hermes-operated relay reachable from any network. Default path for users who don't want to self-host. When `APP_HOSTED_RELAY_ENABLED=true` + `APP_HOSTED_RELAY_URL` are configured in the build, the managed option appears in onboarding and settings.

**Push:** When `APP_PUSH_TRANSPORT=relay` and `APP_PUSH_BROKER_URL` are set, the app attests itself via App Attest against the broker before the broker issues an opaque `relayHandle` + `sendGrant`. Only those opaque handles leave the broker; raw APNs tokens never reach the relay.

**Queueing:** When the host is offline but the relay is reachable, messages queue on the relay and deliver when the connector reconnects. The chat banner shows "Messages can queue while your Hermes host reconnects."

**Failure modes:**
- Relay unreachable (transient network blip or outage): chat refuses new sends with "Hermes relay is unreachable. Check your connection and try again." User can retry from the banner.
- Push broker down: new device registrations fail, but existing `sendGrant`s keep working until they expire. No automatic rotation yet (see [Phase 4 in 1.1plan.md](../1.1plan.md)).

## Self-Hosted Tailscale

User runs a Hermes relay on their own Mac or trusted host and reaches it over a tailnet URL (`https://my-mac.tail-scale.ts.net/v1`) or a tailnet IP. The iOS app treats the tailnet URL exactly like any other custom relay base URL — no tailnet-specific networking code on the client.

**Push:** Not official. Apple-issued APNs tokens only flow to the managed push broker, never to a user-run relay. Delivery happens when the app is foregrounded or reconnected to the tailnet. Users who need real background wake must use Managed Relay.

**Onboarding copy:** The relay URL hint shows a tailnet-style URL. The background-delivery note explicitly says: "Tailscale mode stays honest: messages arrive while the app is in the foreground or reconnected on your tailnet. No official background push."

**Unreachable banner:** Banner action deep-links to `tailscale://` so the user can open the Tailscale app quickly. Sends are refused pre-flight with "Can't reach your tailnet relay. Open Tailscale to reconnect, then send again."

**Tailscale Serve (optional):** Users can run `tailscale serve` to expose `localhost:8000` over the tailnet. That preserves the "only tailnet peers" property without opening public ports.

## Self-Hosted Relay URL

User operates a publicly reachable Hermes relay (e.g. on Fly, Railway, their own VPS). The iOS app treats it identically to any custom relay — base URL + the usual pairing handshake.

**Push:** Same as Tailscale — no official APNs delegation. The long-term path is Phase 4 in the 1.1 plan: eligible self-hosted relays register with the managed push broker and receive `relayHandle`/`sendGrant` pairs just like managed deployments. Until then, these deployments rely on the relay's own notification channel or foreground reconnect.

**Background-delivery note:** "Self-hosted relays don't receive official push credentials. Background delivery depends on your relay's own notification channel."

**Unreachable banner:** "Your self-hosted relay URL is not reachable. Check the URL in Settings and try again." Banner action is a straight retry — no deep-link because the failure mode is the relay itself, not the user's network.

## How the app decides which mode is active

1. `UserSettings.relayConfiguration.connectionMode` is the source of truth (`.managedRelay`, `.tailscale`, `.selfHostedRelay`).
2. `RelayConfiguration.activeBaseURLString` resolves to either `hostedRelayBaseURL` (managed) or `customRelayBaseURL` (Tailscale / self-hosted).
3. `selectableConnectionModes` hides managed if the build isn't configured for it.
4. The push broker is only consulted when `reliesOnOfficialPushRelay == true` (managed only) AND `AppBuildConfiguration.usesManagedPushBroker == true`.

## Legacy migration

Pre-1.1 installs stored `RelayMode.hosted | .custom` in `UserSettings`. On first launch after upgrade, `RelayConnectionMode(legacyRelayMode:)` maps `hosted → managedRelay` and `custom → selfHostedRelay`. Tailscale is never auto-selected; users must opt in explicitly.
