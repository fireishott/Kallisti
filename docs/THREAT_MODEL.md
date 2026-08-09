# Threat Model

This document lays out who we trust, what each component can do, and what a compromise of any one piece gives an attacker. It is scoped to the iOS + relay + push broker + connector topology described in [PRODUCTION_ARCHITECTURE.md](PRODUCTION_ARCHITECTURE.md).

## Actors

| Actor | Capability |
| --- | --- |
| End user | Runs the official iOS app (or a self-built debug app), owns a Mac running the connector |
| Hermes operator | Runs the managed relay + push broker + has custody of the APNs `.p8` |
| Self-hosted operator | Same user or a technical friend/coworker running their own relay |
| Network attacker | Can MITM plaintext traffic, cannot break TLS without a certificate |
| Relay attacker | Has RCE or DB access on a relay (managed or self-hosted) |
| Broker attacker | Has RCE or DB access on the Hermes-managed push broker |
| Device attacker | Has physical access to an unlocked iOS device or Mac |

## Assets

1. **User conversations and connector outputs.** The highest-value asset. Includes prompts, tool results, and anything the Hermes runtime produced on the user's Mac.
2. **Connector-local data.** Sensor SQLite, project files, OpenAI keys, and credentials the user handed the local runtime.
3. **APNs `.p8` credential.** A single private key that can send arbitrary pushes to *every* user of the bundle ID. This is the biggest blast-radius asset in the system.
4. **Device APNs tokens.** One per device. Leakage to a third party lets them send pushes to that device as long as the token is valid.
5. **Relay-held session keys.** Bearer tokens that authenticate an iOS app to a specific relay.
6. **Relay identity private key.** Ed25519 signing key per relay deployment; binds that relay's push-broker sends to that identity.
7. **App Attest public keys.** Per-install, per-device; the broker stores them and uses them to gate re-registration.

## Trust boundaries

The system is split into four trust domains. Crossings are the only places untrusted data enters a trusted zone.

```
┌──────────────────┐    TLS + session key    ┌──────────────────┐
│  iOS app         │ ──────────────────────▶ │  Relay           │
│  (trusted)       │                         │  (semi-trusted)  │
└─────┬────────────┘                         └─────┬────────────┘
      │                                            │
      │ App Attest + TLS                           │ signed push send
      ▼                                            ▼
┌──────────────────┐                         ┌──────────────────┐
│  Push broker     │                         │  Connector       │
│  (trusted)       │                         │  (trusted by     │
│                  │                         │   owner only)    │
└──────────────────┘                         └──────────────────┘
```

- **iOS app** is trusted by the user who installed it. It holds Keychain session keys and grants.
- **Push broker** is trusted by Hermes (the operator). Holds APNs `.p8` and App Attest state.
- **Relay** is only semi-trusted: the user chose it, but the system is designed so that even a fully compromised relay cannot escalate beyond its own device population.
- **Connector** is trusted by the user who installed it. Runs on their machine, holds their local runtime's outputs and credentials.

Crossings worth calling out:

- iOS → relay: mutual knowledge of the session key (set during pairing), always over TLS.
- iOS → push broker: attested over App Attest, one-way only (app initiates).
- Relay → push broker: signed with the relay's Ed25519 identity key; broker verifies `relayHandle` + `sendGrantHash` + signature together.
- Relay → connector: single durable WebSocket; connector initiates the connection outbound, relay never dials the connector.

## Attack scenarios

### 1. Compromised relay (managed or self-hosted)

Worst-case assumption: attacker has DB access and can execute code in the relay's process.

**Can do:**
- Read queued messages and SSE history for that relay's users.
- Enqueue jobs for any connector connected to that relay.
- Send pushes *to devices that registered with this relay* via the broker (requires the stored `sendGrant` + the relay's Ed25519 private key, both of which are on the compromised host).
- Hand out spoofed session keys on pairing for new users.

**Cannot do:**
- Send pushes to users who never paired with this relay. `sendGrant`s are scoped to `(installationId, relayIdentity)` tuples at the broker.
- Read APNs `.p8` or raw APNs tokens — those live on the broker.
- Decrypt OpenAI Realtime audio/vision — that traffic is iOS ↔ OpenAI directly.
- Execute code on the user's Mac beyond what the Hermes runtime's tool permissions already allow. The connector decides what jobs it runs; a malicious relay can only send job *descriptions*.
- Impersonate a different relay to the broker. The broker binds each grant to the attesting relay's `relayIdentity`.

**Mitigations:**
- iOS apps rotate session keys on pairing; old relays lose control when the user re-pairs.
- Relay push grants are short-lived (see Phase 4 rotation work) and the broker can revoke a `relayId`'s grants without touching other relays.
- Connector enforces per-tool allowlists independently of what the relay asks for.

### 2. Compromised push broker

Worst-case assumption: attacker has the APNs `.p8` and the full broker database.

**Can do:**
- Send arbitrary APNs pushes to every registered device for this bundle ID.
- Correlate `relayHandle`s back to raw APNs tokens.
- Revoke or issue fresh `sendGrant`s.

**Cannot do:**
- Read conversation content, user messages, or connector output — the broker is deliberately off-path for all of those. Its only role is "given an opaque handle + a valid grant + a valid signature, fan out to APNs."
- Execute code on a user's Mac.
- Impersonate the iOS app against the broker's *own* App Attest checks without also having Apple's signing keys (App Attest binds attestation to Apple's root).

**Mitigations:**
- Only the Hermes operator runs the broker. Keeping it small and stateless (apart from registrations) shrinks the attack surface.
- Push payloads are intentionally low-information — enough to wake the app, not enough to leak message bodies. The app fetches real content from the relay over its session-key-authenticated channel.
- `.p8` rotation requires re-attesting all installs, which is expensive but recoverable.

### 3. Malicious third-party app trying to register with the broker

Worst-case assumption: an attacker builds their own iOS app and tries to get `relayHandle` + `sendGrant` pairs from the broker (so they can abuse the push fan-out to spam arbitrary devices).

**Defense:** App Attest. The broker demands:

1. A valid App Attest attestation chaining to Apple's root, with the Apple-issued challenge nonce baked into the CBOR.
2. The attestation's `aaguid` matches the broker's configured environment (`appattestdevelop` vs production).
3. The attestation's RP ID hash matches the bundle ID the broker is configured for.
4. The assertion covers the full registration payload (challenge, install ID, token, relay identity) so an attacker can't rebind fields after the fact.
5. The bundle ID is on the broker's allowlist.

An unofficial build cannot produce a valid attestation for the official bundle ID because Apple's App Attest service binds attestations to the team + bundle ID of the running app. Even if the attacker gets a real attestation for *their own* bundle, step 5 rejects it.

**Residual risk:** A jailbroken device could theoretically forge attestations. The broker treats a flood of failed attestations as a signal to rate-limit / flag the environment.

### 4. Compromised iOS device

Worst-case assumption: attacker has an unlocked device with the official app installed, or can read Keychain contents.

**Can do:**
- Read that user's session key, `relayHandle`, `sendGrant`, and conversation history cached on the device.
- Send pushes to *their own device* (not others).
- Send messages to that user's connector through that user's relay (same as the legitimate user).

**Cannot do:**
- Send pushes to other users. `sendGrant`s are install-scoped.
- Extract the relay identity private key (lives on the relay, not the device).
- Cross-correlate install IDs across devices — each App Attest key is per-install.

**Mitigations:**
- iOS Keychain + device passcode are the baseline defense.
- Losing/replacing a device re-triggers App Attest registration on first launch, invalidating the prior install's push grants once the broker expires them.

### 5. Compromised connector (user's Mac)

This is the user's own machine. If it's compromised, the user's local data is already compromised — the system architecture cannot help here and doesn't try to.

**Relevant:** the compromise cannot propagate *out* of that Mac into Hermes infrastructure, because the connector only initiates outbound WebSockets to the relay. A malicious connector can send garbage to the relay, but the relay doesn't execute connector-supplied code.

### 6. Network attacker between iOS and relay

**Can do:** Everything TLS doesn't stop — count packets, measure timing, observe SNI. Cannot read bodies or forge session-keyed requests without the session key.

**Cannot do:** MITM session-keyed traffic without a cert trusted by the device.

**Mitigations:** All HTTP is strict TLS; `fly.toml` and self-hosted guidance require HTTPS.

### 7. Network attacker between relay and push broker

Same as above — TLS terminates at the broker. Additionally, signed push-send requests include a monotonically increasing timestamp and a signature, so replay past expiry is rejected.

## Connection-mode-specific considerations

### Managed Relay
- **Push path:** Hermes-operated end-to-end. Single point of trust (the broker and relay are both operated by Hermes, but remain process-isolated).
- **Residual risk:** Hermes operational security. Documented in the operator runbook.

### Self-Hosted Tailscale
- **Push path:** No official push. The relay is unreachable from the broker by design (tailnet-only), so no `sendGrant` is ever minted for it.
- **Upshot:** The worst a compromised tailnet relay can do is within that user's tailnet. No cross-user blast radius exists because there is no multi-tenant path.

### Self-Hosted Relay URL
- **Push path:** No official push today (Phase 4 will let attested deployments opt into the broker). Until then, background delivery depends on whatever the relay itself implements.
- **Residual risk:** The user chose to run a public relay. A compromise is scoped to that user's session keys and any devices they paired with that relay.

## Non-goals

- **Protecting against malicious users of their own data.** If the user installs a compromised connector, jailbreaks their phone, or pastes their session key into a malicious tool, we cannot recover.
- **Protecting against Apple.** Apple has root trust on the device and in App Attest. The threat model assumes Apple's infrastructure behaves as documented.
- **Anonymity.** Hermes does not attempt to hide which relay a user is on or how often they push. The broker logs per-relay send counts for rate limiting.

## Open items

- Phase 4: grant revocation and rotation endpoints on the broker (currently grants live until their stored `expiresAt`).
- Phase 4: self-hosted relay onboarding against the broker with its own attestation story (probably: owner of the relay signs a TOS via the official app + relay identity key; broker issues grants bounded to that relay's devices).
- Runbook for `.p8` rotation (documented separately once the broker admin surface lands in Phase 6).
- Revisit rate limits on `POST /v1/push-broker/challenge` once we see real traffic — the current limits are conservative guesses.

## Related documents

- [PRODUCTION_ARCHITECTURE.md](PRODUCTION_ARCHITECTURE.md) — the topology this threat model is scoped to.
- [PUSH_RELAY.md](PUSH_RELAY.md) — the registration and send protocols the broker uses.
- [CONNECTION_MODES.md](CONNECTION_MODES.md) — how each mode maps onto this model.
