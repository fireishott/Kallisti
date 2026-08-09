# Managed Push Broker

Hermes' production push path separates the APNs credential holder (the **push broker**) from the Hermes relay, so self-hosted relays never receive official APNs secrets or raw device tokens.

## Goals

1. Only the official managed iOS build can register APNs tokens with the broker.
2. Raw APNs tokens never leave the broker. Relays get opaque `relayHandle` + `sendGrant` pairs instead.
3. A compromised self-hosted relay cannot impersonate a legitimate app instance or send arbitrary pushes to other users' devices.
4. The official app can eventually delegate sends through *any* relay the user controls, without Hermes trusting that relay.

## Roles

| Role | What it holds | What it sees |
| --- | --- | --- |
| iOS app (official build) | APNs token, App Attest key, stored `relayHandle`/`sendGrant` in Keychain | Its own token, its own grants |
| Push broker (Hermes-managed) | APNs `.p8`, mapping of opaque `relayHandle → apnsToken`, App Attest public keys | Raw APNs tokens, attestation receipts |
| Relay (managed or self-hosted) | `relayHandle`, `sendGrant`, relay Ed25519 signing key | Opaque handles only |

## Registration flow

```
iOS                                 Push Broker                       Relay
 │                                       │                              │
 │── POST /v1/push-broker/challenge ────▶│                              │
 │◀───── {challengeId, challenge} ──────│                              │
 │                                       │                              │
 │── App Attest attestation + assertion ▶│                              │
 │   over signed payload that binds:     │                              │
 │   {challengeId, installationId,       │                              │
 │    bundleId, appVersion,              │                              │
 │    apnsEnvironment, apnsToken,        │                              │
 │    relayIdentity{id, publicKey}}      │                              │
 │                                       │                              │
 │◀── {relayHandle, sendGrant, relayId,─│                              │
 │     relayPublicKey, expiresAt}       │                              │
 │                                       │                              │
 │                               (broker stores                         │
 │                                hash(sendGrant),                      │
 │                                raw apnsToken,                        │
 │                                attestation)                          │
 │                                                                      │
 │── POST {relay}/v1/push/register with relayHandle+sendGrant ─────────▶│
 │   (no raw APNs token; transport="relay")                             │
 │                                                                      │
 │                                                    (relay stores     │
 │                                                     only opaque      │
 │                                                     handle+grant)    │
```

The broker validates:

- Challenge is one-time and unexpired.
- App Attest attestation chains to an Apple root, with the correct nonce extension, RP ID hash, and AAGUID environment.
- Assertion signature verifies against the attested public key.
- `apnsEnvironment` matches the configured broker environment and `bundleId` is on the allowlist.

Result: a tuple `{relayHandle, sendGrant, relayId, relayPublicKey, expiresAt}` that the iOS app persists in Keychain and hands to the relay.

## Send flow

```
Relay                                       Push Broker                APNs
 │                                               │                       │
 │── POST /v1/push-broker/send ─────────────────▶│                       │
 │   { relayHandle,                              │                       │
 │     sendGrantHash,                            │                       │
 │     relayId,                                  │                       │
 │     payload,                                  │                       │
 │     expiresAt,                                │                       │
 │     signature (Ed25519 over canonical body)   │                       │
 │   }                                           │                       │
 │                                               │                       │
 │                                     (broker verifies:                 │
 │                                       relayHandle exists,             │
 │                                       sendGrantHash matches,          │
 │                                       relayId + publicKey             │
 │                                         from registration,            │
 │                                       signature verifies,             │
 │                                       grant unexpired)                │
 │                                               │                       │
 │                                               │── APNs HTTP/2 ───────▶│
 │                                               │◀─── 200 / reason ────│
 │◀──── {delivered | reason} ───────────────────│                       │
```

A leaked `sendGrant` on its own is **not enough** to send — the broker requires a valid Ed25519 signature from `relayId`, and that private key lives only on the relay. A leaked relay private key is limited to pushing to devices that already granted that specific relay a send grant.

## Relay identity

Every relay deployment generates a persistent Ed25519 keypair on first start (`relay/app/relay_identity.py`). The public key is served at `GET /v1/relay/identity`. The iOS app reads that identity, includes it in the App Attest–signed payload, and hands it to the broker during registration. This binds each `sendGrant` to a specific relay deployment — a send grant for Relay A cannot be replayed by Relay B.

## Keychain storage (iOS)

`PushBrokerRegistrationStore` persists this per installation + relay identity tuple:

- `relayHandle`, `sendGrant`
- `relayId`, `relayPublicKey` (for cache validation)
- `brokerBaseURL` (cache invalidates if the broker URL changes)
- `installationID`, `tokenHash` (cache invalidates if the APNs token rotates)
- `expiresAt`

`PushRegistrationCoordinator` checks this cache before re-running App Attest. Re-registration happens when any of the cache keys change or the grant is within its expiry window.

## What remains (Phase 4 of the 1.1 plan)

- Registration revocation and grant rotation endpoints.
- Managed broker admin/diagnostic views (Phase 6).
- Self-hosted relay onboarding against the broker (so users who run their own public relay can also get official push delivery after attesting their deployment).

## Related modules

- iOS: [PushBrokerClient.swift](../HermesMobile/Services/Support/PushBrokerClient.swift), [PushRegistrationCoordinator.swift](../HermesMobile/Services/Support/PushRegistrationCoordinator.swift), [AppAttestService.swift](../HermesMobile/Services/Support/AppAttestService.swift), [PushBrokerRegistrationStore.swift](../HermesMobile/Services/Support/PushBrokerRegistrationStore.swift)
- Relay: [push_broker.py](../relay/app/push_broker.py), [app_attest.py](../relay/app/app_attest.py), [relay_identity.py](../relay/app/relay_identity.py)
- Tests: [test_push_broker.py](../relay/tests/test_push_broker.py), [test_app_attest.py](../relay/tests/test_app_attest.py)
